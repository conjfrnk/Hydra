///////////////////////////////////////////////////////////////////////////
// File: src/main.rs
// - Round-robin scheduling of multiple jobs
// - Chunk reassign logic if a worker is too slow (10× average chunk time)
// - Heartbeat inactivity => kill after 30s no poll
// - "Internal polling" every 500ms => store partial Pi in job.samples
// - /api/job_history/<job_id> returns all sampled data
// - E0308 fix => unify both branches to (StatusCode, Json<HistoryResponse>)
///////////////////////////////////////////////////////////////////////////

use anyhow::Result;
use axum::http::StatusCode;
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use hyper::server::conn::Http;
use rustls_pemfile;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::BufReader,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tokio_rustls::rustls::{Certificate, PrivateKey};
use tokio_rustls::TlsAcceptor;

///////////////////////////////////////////////////////////////////////////
// 1) AppState, Data Structures
///////////////////////////////////////////////////////////////////////////

#[derive(Clone)]
struct AppState {
    jobs: Arc<Mutex<HashMap<String, JobInfo>>>,
    workers: Arc<Mutex<HashMap<String, WorkerStats>>>,
    run_queue: Arc<Mutex<Vec<String>>>,
    round_counter: Arc<Mutex<u64>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct JobInfo {
    job_id: String,
    status: String,         // "in-progress", "finished", or "error"
    result: String,         // final Pi if finished
    partial_result: String, // partial Pi if in-progress
    percent_complete: f32,

    points: u64,
    chunk_size: u64,
    completed_chunks: u64,
    total_chunks: u64,
    points_in_circle: u64,
    points_total: u64,

    created_at: u64,
    next_chunk: u64,

    average_chunk_time: f64,
    chunks_in_progress: HashMap<u64, ChunkAssignment>,
    last_heartbeat: u64, // updated each time /job_status is called

    // The list of partial Pi samples we accumulate every 500ms in background
    samples: Vec<SamplePoint>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChunkAssignment {
    worker_id: String,
    assigned_at: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WorkerStats {
    worker_id: String,
    total_compute: u64,
}

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    job_id: String,
    task_type: String,
    points: u64,
}

#[derive(Debug, Serialize)]
struct CreateJobResponse {
    job_id: String,
    status: String,
}

// For "normal" get_job_status
#[derive(Debug, Serialize)]
struct JobStatusResponse {
    status: String,
    result: String,
    partial_result: String,
    percent_complete: f32,
}

#[derive(Debug, Deserialize)]
struct RegisterWorkerRequest {
    worker_id: String,
}

#[derive(Debug, Serialize)]
struct RegisterWorkerResponse {
    status: String,
    assigned_worker_id: String,
}

#[derive(Debug, Deserialize)]
struct SubmitChunkRequest {
    worker_id: String,
    job_id: String,
    points_in_circle: u64,
    chunk_points: u64,
}

#[derive(Debug, Serialize)]
struct SubmitChunkResponse {
    status: String,
    updated_worker_total: u64,
}

#[derive(Debug, Serialize)]
struct AvailableJobResponse {
    job_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct AssignChunkResponse {
    chunk_index: Option<u64>,
    chunk_size: u64,
    job_status: String,
}

#[derive(Debug, Serialize)]
struct MarkErrorResponse {
    job_id: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct HistoryResponse {
    samples: Vec<SamplePoint>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct SamplePoint {
    percent: f32,
    approx_pi: f64,
}

///////////////////////////////////////////////////////////////////////////
// 2) Main
///////////////////////////////////////////////////////////////////////////

#[tokio::main]
async fn main() -> Result<()> {
    let state = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        workers: Arc::new(Mutex::new(HashMap::new())),
        run_queue: Arc::new(Mutex::new(Vec::new())),
        round_counter: Arc::new(Mutex::new(0)),
    };

    // 1) Start background sampling => store partial data every 500ms
    {
        let sampling_jobs = state.jobs.clone();
        tokio::spawn(async move {
            sampling_task(sampling_jobs).await;
        });
    }

    // 2) Start background cleanup => heartbeat inactivity + chunk reassign
    {
        let jobs_for_cleanup = state.jobs.clone();
        let queue_for_cleanup = state.run_queue.clone();
        tokio::spawn(async move {
            cleanup_inactive_jobs(jobs_for_cleanup, queue_for_cleanup).await;
        });
    }

    println!("[Scheduler] Loading certs/keys");
    let certs = load_certs("certs/scheduler_cert.pem")?;
    let key = load_private_key("certs/scheduler_key.pem")?;
    let tls_config = build_tls_config(certs, key)?;

    let app = Router::new()
        .route("/api/create_job", post(create_job))
        .route("/api/job_status/:job_id", get(get_job_status))
        .route("/api/register_worker", post(register_worker))
        .route("/api/submit_chunk", post(submit_chunk))
        .route("/api/available_job", get(available_job))
        .route("/api/assign_chunk/:job_id", get(assign_chunk))
        .route("/api/mark_job_error/:job_id", post(mark_job_error))
        // new route => /api/job_history/:job_id
        .route("/api/job_history/:job_id", get(get_job_history))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 8443));
    println!("[Scheduler] Binding to https://{}", addr);
    let listener = TcpListener::bind(addr).await?;
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));

    println!("[Scheduler] Now listening...");
    loop {
        let (stream, _) = listener.accept().await?;
        let acceptor = acceptor.clone();
        let app_clone = app.clone();

        tokio::spawn(async move {
            match acceptor.accept(stream).await {
                Ok(tls_stream) => {
                    let _ = Http::new().serve_connection(tls_stream, app_clone).await;
                }
                Err(e) => {
                    eprintln!("[Scheduler] TLS handshake error: {:?}", e);
                }
            }
        });
    }
}

///////////////////////////////////////////////////////////////////////////
// 3) Endpoints
///////////////////////////////////////////////////////////////////////////

/////////////////////////////////////
// 3.1 create_job
/////////////////////////////////////
async fn create_job(
    State(state): State<AppState>,
    Json(payload): Json<CreateJobRequest>,
) -> Json<CreateJobResponse> {
    println!("[Scheduler] create_job: {:?}", payload);

    let mut jobs = state.jobs.lock().unwrap();
    let mut queue = state.run_queue.lock().unwrap();

    let chunk_size = 100_000;
    let total_chunks = (payload.points + chunk_size - 1) / chunk_size;

    let job_info = JobInfo {
        job_id: payload.job_id.clone(),
        status: "in-progress".to_string(),
        result: "".to_string(),
        partial_result: "".to_string(),
        percent_complete: 0.0,

        points: payload.points,
        chunk_size,
        completed_chunks: 0,
        total_chunks,
        points_in_circle: 0,
        points_total: 0,
        created_at: current_timestamp(),
        next_chunk: 0,

        average_chunk_time: 5.0,
        chunks_in_progress: HashMap::new(),
        last_heartbeat: current_timestamp(),
        samples: Vec::new(),
    };

    jobs.insert(payload.job_id.clone(), job_info);
    queue.push(payload.job_id.clone());

    println!(
        "[Scheduler] Created job_id={} total_chunks={}",
        payload.job_id, total_chunks
    );
    Json(CreateJobResponse {
        job_id: payload.job_id,
        status: "created".to_string(),
    })
}

/////////////////////////////////////
// 3.2 get_job_status
/////////////////////////////////////
async fn get_job_status(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let mut jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get_mut(&job_id) {
        if job.status == "in-progress" {
            job.last_heartbeat = current_timestamp();
        }
        let resp = JobStatusResponse {
            status: job.status.clone(),
            result: job.result.clone(),
            partial_result: job.partial_result.clone(),
            percent_complete: job.percent_complete,
        };
        Json(resp)
    } else {
        Json(JobStatusResponse {
            status: "error".to_string(),
            result: "".to_string(),
            partial_result: "job_id not found".to_string(),
            percent_complete: 0.0,
        })
    }
}

/////////////////////////////////////
// 3.3 register_worker
/////////////////////////////////////
async fn register_worker(
    State(state): State<AppState>,
    Json(payload): Json<RegisterWorkerRequest>,
) -> Json<RegisterWorkerResponse> {
    println!("[Scheduler] register_worker: {:?}", payload);

    let mut workers = state.workers.lock().unwrap();
    workers
        .entry(payload.worker_id.clone())
        .or_insert(WorkerStats {
            worker_id: payload.worker_id.clone(),
            total_compute: 0,
        });

    Json(RegisterWorkerResponse {
        status: "registered".to_string(),
        assigned_worker_id: payload.worker_id,
    })
}

/////////////////////////////////////
// 3.4 submit_chunk
/////////////////////////////////////
async fn submit_chunk(
    State(state): State<AppState>,
    Json(payload): Json<SubmitChunkRequest>,
) -> Json<SubmitChunkResponse> {
    println!("[Scheduler] submit_chunk: {:?}", payload);

    let now = current_timestamp();
    let mut jobs = state.jobs.lock().unwrap();
    let mut queue = state.run_queue.lock().unwrap();

    if let Some(job) = jobs.get_mut(&payload.job_id) {
        if job.status == "finished" {
            return Json(SubmitChunkResponse {
                status: "job_already_finished".to_string(),
                updated_worker_total: 0,
            });
        }
        if job.status == "error" {
            return Json(SubmitChunkResponse {
                status: "job_in_error".to_string(),
                updated_worker_total: 0,
            });
        }

        job.last_heartbeat = current_timestamp();

        let chunk_idx = if job.next_chunk == 0 { 0 } else { job.next_chunk - 1 };
        if let Some(assignment) = job.chunks_in_progress.remove(&chunk_idx) {
            let elapsed_sec = (now.saturating_sub(assignment.assigned_at)) as f64;
            let alpha = 0.3;
            job.average_chunk_time =
                alpha * elapsed_sec + (1.0 - alpha) * job.average_chunk_time;
        }

        job.completed_chunks += 1;
        job.points_in_circle += payload.points_in_circle;
        job.points_total += payload.chunk_points;

        let partial_pi = 4.0 * (job.points_in_circle as f64 / job.points_total as f64);
        job.partial_result = format!("{:.6}", partial_pi);
        job.percent_complete = (job.points_total as f32 / job.points as f32) * 100.0;

        // If we've reached or exceeded total points => finished
        if job.points_total >= job.points {
            // 1) push a final sample at 100%
            let final_pi = job.partial_result.parse::<f64>().unwrap_or(0.0);
            job.samples.push(SamplePoint {
                percent: 100.0,
                approx_pi: final_pi,
            });

            job.status = "finished".to_string();
            job.result = job.partial_result.clone(); // final
            job.percent_complete = 100.0;

            println!(
                "[Scheduler] job_id={} => FINISHED. final pi={}",
                payload.job_id, job.result
            );
            remove_from_queue(&mut queue, &payload.job_id);
        } else {
            // job is still in progress
            println!(
                "[Scheduler] job_id={} => partial pi={}, {}% done",
                payload.job_id, job.partial_result, job.percent_complete
            );
        }
    } else {
        println!(
            "[Scheduler] job_id={} not found in submit_chunk",
            payload.job_id
        );
        return Json(SubmitChunkResponse {
            status: "not_found".to_string(),
            updated_worker_total: 0,
        });
    }

    let mut workers = state.workers.lock().unwrap();
    let updated_total = if let Some(w) = workers.get_mut(&payload.worker_id) {
        w.total_compute += 1;
        w.total_compute
    } else {
        0
    };

    Json(SubmitChunkResponse {
        status: "chunk_submitted".to_string(),
        updated_worker_total: updated_total,
    })
}

/////////////////////////////////////
// 3.5 available_job
/////////////////////////////////////
async fn available_job(State(state): State<AppState>) -> Json<AvailableJobResponse> {
    let jobs = state.jobs.lock().unwrap();
    let queue = state.run_queue.lock().unwrap();
    let mut counter = state.round_counter.lock().unwrap();

    let in_progress: Vec<_> = queue
        .iter()
        .filter_map(|jid| {
            let job = jobs.get(jid)?;
            if job.status == "in-progress" {
                Some(jid.clone())
            } else {
                None
            }
        })
        .collect();

    if in_progress.is_empty() {
        return Json(AvailableJobResponse { job_id: None });
    }

    let idx = (*counter as usize) % in_progress.len();
    *counter += 1;
    let chosen = in_progress[idx].clone();

    Json(AvailableJobResponse {
        job_id: Some(chosen),
    })
}

/////////////////////////////////////
// 3.6 assign_chunk
/////////////////////////////////////
async fn assign_chunk(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Result<Json<AssignChunkResponse>, (StatusCode, &'static str)> {
    let now = current_timestamp();
    let mut jobs = state.jobs.lock().unwrap();
    let mut queue = state.run_queue.lock().unwrap();

    if let Some(job) = jobs.get_mut(&job_id) {
        if job.status == "finished" {
            return Ok(Json(AssignChunkResponse {
                chunk_index: None,
                chunk_size: 0,
                job_status: "finished".to_string(),
            }));
        }
        if job.status == "error" {
            return Ok(Json(AssignChunkResponse {
                chunk_index: None,
                chunk_size: 0,
                job_status: "error".to_string(),
            }));
        }

        job.last_heartbeat = current_timestamp();

        if job.next_chunk >= job.total_chunks {
            job.status = "finished".to_string();
            job.result = job.partial_result.clone();
            job.percent_complete = 100.0;
            remove_from_queue(&mut queue, &job_id);
            return Ok(Json(AssignChunkResponse {
                chunk_index: None,
                chunk_size: 0,
                job_status: "finished".to_string(),
            }));
        }

        let cindex = job.next_chunk;
        job.next_chunk += 1;

        job.chunks_in_progress.insert(
            cindex,
            ChunkAssignment {
                worker_id: "unknown".to_string(),
                assigned_at: now,
            },
        );

        Ok(Json(AssignChunkResponse {
            chunk_index: Some(cindex),
            chunk_size: job.chunk_size,
            job_status: job.status.clone(),
        }))
    } else {
        Err((StatusCode::NOT_FOUND, "Job not found"))
    }
}

/////////////////////////////////////
// 3.7 mark_job_error
/////////////////////////////////////
async fn mark_job_error(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let mut jobs = state.jobs.lock().unwrap();
    let mut queue = state.run_queue.lock().unwrap();

    match jobs.get_mut(&job_id) {
        Some(job) => {
            job.status = "error".to_string();
            remove_from_queue(&mut queue, &job_id);
            println!("[Scheduler] job_id={} forcibly => error", job_id);
            Json(MarkErrorResponse {
                job_id,
                status: "error".to_string(),
            })
        }
        None => {
            let new_job = JobInfo {
                job_id: job_id.clone(),
                status: "error".to_string(),
                result: "".to_string(),
                partial_result: "".to_string(),
                percent_complete: 0.0,
                points: 0,
                chunk_size: 0,
                completed_chunks: 0,
                total_chunks: 0,
                points_in_circle: 0,
                points_total: 0,
                created_at: current_timestamp(),
                next_chunk: 0,
                average_chunk_time: 5.0,
                chunks_in_progress: HashMap::new(),
                last_heartbeat: current_timestamp(),
                samples: Vec::new(),
            };
            jobs.insert(job_id.clone(), new_job);
            remove_from_queue(&mut queue, &job_id);

            println!(
                "[Scheduler] job_id={} not found => created error record",
                job_id
            );
            Json(MarkErrorResponse {
                job_id,
                status: "error".to_string(),
            })
        }
    }
}

/////////////////////////////////////
// 3.8 get_job_history => returns job.samples
/////////////////////////////////////
async fn get_job_history(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get(&job_id) {
        // Return (StatusCode, Json<HistoryResponse>) in if branch
        let resp = HistoryResponse {
            samples: job.samples.clone(),
        };
        (StatusCode::OK, Json(resp))
    } else {
        // Return same type in else => (StatusCode, Json<HistoryResponse>)
        (StatusCode::NOT_FOUND, Json(HistoryResponse { samples: Vec::new() }))
    }
}

///////////////////////////////////////////////////////////////////////////
// 4) Background tasks
///////////////////////////////////////////////////////////////////////////
async fn cleanup_inactive_jobs(
    jobs: Arc<Mutex<HashMap<String, JobInfo>>>,
    run_queue: Arc<Mutex<Vec<String>>>,
) {
    loop {
        {
            let mut map = jobs.lock().unwrap();
            let mut queue = run_queue.lock().unwrap();
            let now = current_timestamp();

            for (job_id, job) in map.iter_mut() {
                if job.status == "in-progress" {
                    // heartbeat inactivity => 30s
                    let elapsed_since_heartbeat = now.saturating_sub(job.last_heartbeat);
                    if elapsed_since_heartbeat > 30 {
                        println!(
                            "[Scheduler] job_id={} => canceled (heartbeat inactivity).",
                            job_id
                        );
                        job.status = "error".to_string();
                        job.result.clear();
                        job.partial_result.clear();
                        job.percent_complete = 0.0;
                        job.points_in_circle = 0;
                        job.points_total = 0;
                        remove_from_queue(&mut queue, job_id);
                        continue;
                    }

                    // chunk-timeout => 10× average
                    let time_limit = 10.0 * job.average_chunk_time;
                    let mut reassign_list = vec![];
                    for (&cidx, assignment) in job.chunks_in_progress.iter() {
                        let elapsed_sec = now.saturating_sub(assignment.assigned_at) as f64;
                        if elapsed_sec > time_limit {
                            println!(
                                "[Scheduler] job_id={} chunk={} overdue ({}s > 10×{}s). Reassigning.",
                                job_id, cidx, elapsed_sec, job.average_chunk_time
                            );
                            reassign_list.push(cidx);
                        }
                    }
                    for cidx in reassign_list {
                        job.chunks_in_progress.remove(&cidx);
                        if cidx < job.next_chunk {
                            job.next_chunk = cidx;
                        }
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// sample partial data every 500ms
async fn sampling_task(jobs: Arc<Mutex<HashMap<String, JobInfo>>>) {
    loop {
        {
            let mut map = jobs.lock().unwrap();
            for (_jid, job) in map.iter_mut() {
                if job.status == "in-progress" && job.points_total > 0 {
                    // parse partial_result
                    let pi_val = job.partial_result.parse::<f64>().unwrap_or(0.0);
                    let pct = job.percent_complete;
                    job.samples.push(SamplePoint {
                        approx_pi: pi_val,
                        percent: pct,
                    });
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

///////////////////////////////////////////////////////////////////////////
// 5) Helpers
///////////////////////////////////////////////////////////////////////////
fn remove_from_queue(queue: &mut Vec<String>, job_id: &str) {
    if let Some(pos) = queue.iter().position(|x| x == job_id) {
        queue.remove(pos);
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load_certs(path: &str) -> Result<Vec<Certificate>> {
    println!("[Scheduler] Loading certs from: {}", path);
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut certs = Vec::new();
    while let Some(item) = rustls_pemfile::read_one(&mut reader)? {
        if let rustls_pemfile::Item::X509Certificate(b) = item {
            certs.push(Certificate(b))
        }
    }
    Ok(certs)
}

fn load_private_key(path: &str) -> Result<PrivateKey> {
    println!("[Scheduler] Loading private key from: {}", path);
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    while let Some(item) = rustls_pemfile::read_one(&mut reader)? {
        match item {
            rustls_pemfile::Item::PKCS8Key(b) => return Ok(PrivateKey(b)),
            rustls_pemfile::Item::RSAKey(b) => return Ok(PrivateKey(b)),
            _ => {}
        }
    }
    Err(anyhow::anyhow!("No private key found"))
}

fn build_tls_config(certs: Vec<Certificate>, key: PrivateKey) -> Result<rustls::ServerConfig> {
    use rustls::ServerConfig;
    println!("[Scheduler] Building TLS config");
    let cfg = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    Ok(cfg)
}
