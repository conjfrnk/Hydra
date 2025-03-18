//! Hydra - Scheduler main entry point
//! Copyright (C) 2025 Connor Frank
//!
//! This program is free software: you can redistribute it and/or modify
//! it under the terms of the GNU General Public License as published by
//! the Free Software Foundation, either version 3 of the License, or
//! (at your option) any later version.
//!
//! This program is distributed in the hope that it will be useful,
//! but WITHOUT ANY WARRANTY; without even the implied warranty of
//! MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
//! GNU General Public License for more details.
//!
//! You should have received a copy of the GNU General Public License
//! along with this program. If not, see <https://www.gnu.org/licenses/>.

use anyhow::Result;
use axum::http::StatusCode;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    response::IntoResponse,
    routing::{get, post},
};
use hyper::server::conn::Http;
use rustls_pemfile;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::BufReader,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{Certificate, PrivateKey};

/////////////////////////////////////
// AppState, Data
/////////////////////////////////////

#[derive(Clone)]
struct AppState {
    jobs: Arc<Mutex<HashMap<String, JobInfo>>>,
    workers: Arc<Mutex<HashMap<String, WorkerStats>>>,
    run_queue: Arc<Mutex<Vec<String>>>,
    round_counter: Arc<Mutex<u64>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct WorkerStats {
    worker_id: String,
    total_compute: u64,
    avg_points_per_sec: f64,
    measure_count: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct JobInfo {
    job_id: String,
    status: String, // "in-progress", "paused", "finished", "error"
    result: String,
    partial_result: String,
    percent_complete: f32,

    points: u64,
    chunk_size: u64, // base chunk size; used for clamping
    completed_chunks: u64,
    total_chunks: u64,
    points_in_circle: u64,
    points_total: u64,

    created_at: u64,
    next_chunk: u64,

    average_chunk_time: f64,
    chunks_in_progress: HashMap<u64, ChunkAssignment>,

    last_webapp_poll: u64,
    samples: Vec<SamplePoint>,
    killed_chunks: HashSet<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChunkAssignment {
    worker_id: String,
    assigned_at: u64,
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
    chunk_index: u64,
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

#[derive(Debug, Deserialize)]
struct AssignChunkQuery {
    worker_id: String, // e.g. "?worker_id=my_single_worker"
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

/////////////////////////////////////
// main
/////////////////////////////////////
#[tokio::main]
async fn main() -> Result<()> {
    let state = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        workers: Arc::new(Mutex::new(HashMap::new())),
        run_queue: Arc::new(Mutex::new(Vec::new())),
        round_counter: Arc::new(Mutex::new(0)),
    };

    // sampling partial Pi every 500ms
    {
        let sampling_jobs = state.jobs.clone();
        tokio::spawn(async move {
            sampling_task(sampling_jobs).await;
        });
    }

    // background => after 10s => paused, after 300s => error
    {
        let jobs_for_cleanup = state.jobs.clone();
        let queue_for_cleanup = state.run_queue.clone();
        tokio::spawn(async move {
            cleanup_inactive_jobs(jobs_for_cleanup, queue_for_cleanup).await;
        });
    }

    println!("[Scheduler] Loading TLS certs/keys");
    let certs = load_certs("certs/scheduler_cert.pem")?;
    let key = load_private_key("certs/scheduler_key.pem")?;
    let tls_config = build_tls_config(certs, key)?;

    let app = Router::new()
        .route("/api/create_job", post(create_job))
        .route("/api/job_status/:job_id", get(get_job_status))
        .route("/api/register_worker", post(register_worker))
        .route("/api/submit_chunk", post(submit_chunk))
        .route("/api/available_job", get(available_job))
        // now we require worker_id in the query string
        .route("/api/assign_chunk/:job_id", get(assign_chunk))
        .route("/api/mark_job_error/:job_id", post(mark_job_error))
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

/////////////////////////////////////
// create_job => last_webapp_poll=now
/////////////////////////////////////
async fn create_job(
    State(state): State<AppState>,
    Json(payload): Json<CreateJobRequest>,
) -> Json<CreateJobResponse> {
    println!("[Scheduler] create_job: {:?}", payload);
    let mut jobs = state.jobs.lock().unwrap();
    let mut queue = state.run_queue.lock().unwrap();

    let base_chunk_size = 100_000;
    let total_chunks = (payload.points + base_chunk_size - 1) / base_chunk_size;
    let now = current_timestamp();

    let job_info = JobInfo {
        job_id: payload.job_id.clone(),
        status: "in-progress".to_string(),
        result: "".to_string(),
        partial_result: "".to_string(),
        percent_complete: 0.0,

        points: payload.points,
        chunk_size: base_chunk_size,
        completed_chunks: 0,
        total_chunks,
        points_in_circle: 0,
        points_total: 0,
        created_at: now,
        next_chunk: 0,

        average_chunk_time: 5.0,
        chunks_in_progress: HashMap::new(),
        last_webapp_poll: now,
        samples: Vec::new(),
        killed_chunks: HashSet::new(),
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
// get_job_status => update last_webapp_poll
/////////////////////////////////////
async fn get_job_status(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let mut jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get_mut(&job_id) {
        job.last_webapp_poll = current_timestamp();

        if job.status == "paused" {
            job.status = "in-progress".to_string();
            println!("[Scheduler] job_id={} => resumed via webapp poll", job_id);

            let mut queue = state.run_queue.lock().unwrap();
            if !queue.contains(&job_id) {
                queue.push(job_id.clone());
            }
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
// register_worker
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
            avg_points_per_sec: 0.0,
            measure_count: 0,
        });

    println!("[Scheduler] Worker {} => registered", payload.worker_id);

    Json(RegisterWorkerResponse {
        status: "registered".to_string(),
        assigned_worker_id: payload.worker_id,
    })
}

/////////////////////////////////////
// submit_chunk => measure time => update worker stats => partial result
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
        match job.status.as_str() {
            "finished" => {
                return Json(SubmitChunkResponse {
                    status: "job_already_finished".to_string(),
                    updated_worker_total: 0,
                });
            }
            "error" => {
                return Json(SubmitChunkResponse {
                    status: "job_in_error".to_string(),
                    updated_worker_total: 0,
                });
            }
            "paused" => {
                println!(
                    "[Scheduler] job_id={} => chunk arrived while paused",
                    payload.job_id
                );
            }
            _ => {}
        }

        if job.killed_chunks.contains(&payload.chunk_index) {
            println!(
                "[Scheduler] ignoring chunk={} for job={} (killed)",
                payload.chunk_index, payload.job_id
            );
            return Json(SubmitChunkResponse {
                status: "chunk_killed".to_string(),
                updated_worker_total: 0,
            });
        }

        let mut elapsed_sec = 0.0;
        if let Some(assignment) = job.chunks_in_progress.remove(&payload.chunk_index) {
            elapsed_sec = now.saturating_sub(assignment.assigned_at) as f64;
            let alpha = 0.3;
            job.average_chunk_time = alpha * elapsed_sec + (1.0 - alpha) * job.average_chunk_time;
        }

        job.completed_chunks += 1;
        job.points_in_circle += payload.points_in_circle;
        job.points_total += payload.chunk_points;

        let partial_pi = 4.0 * (job.points_in_circle as f64 / job.points_total as f64);
        job.partial_result = format!("{:.6}", partial_pi);
        job.percent_complete = (job.points_total as f32 / job.points as f32) * 100.0;

        if job.points_total >= job.points {
            let final_val = job.partial_result.parse::<f64>().unwrap_or(0.0);
            job.samples.push(SamplePoint {
                approx_pi: final_val,
                percent: 100.0,
            });
            job.status = "finished".to_string();
            job.result = job.partial_result.clone();
            job.percent_complete = 100.0;
            println!(
                "[Scheduler] job_id={} => FINISHED, final pi={}",
                payload.job_id, job.result
            );
            remove_from_queue(&mut queue, &payload.job_id);
        } else {
            println!(
                "[Scheduler] job_id={} => partial pi={}, {}% done",
                payload.job_id, job.partial_result, job.percent_complete
            );
        }

        let mut workers_map = state.workers.lock().unwrap();
        let updated_total = if let Some(w) = workers_map.get_mut(&payload.worker_id) {
            w.total_compute += 1;

            if elapsed_sec > 0.0 && payload.chunk_points > 0 {
                let pps = payload.chunk_points as f64 / elapsed_sec;
                let blend = 0.3;
                if w.measure_count == 0 {
                    w.avg_points_per_sec = pps;
                } else {
                    w.avg_points_per_sec = (1.0 - blend) * w.avg_points_per_sec + blend * pps;
                }
                w.measure_count += 1;

                println!(
                    "[Scheduler] Worker {} => measured pps={:.2}, new_avg={:.2}",
                    w.worker_id, pps, w.avg_points_per_sec
                );
            }

            w.total_compute
        } else {
            0
        };

        Json(SubmitChunkResponse {
            status: "chunk_submitted".to_string(),
            updated_worker_total: updated_total,
        })
    } else {
        println!(
            "[Scheduler] job_id={} not found in submit_chunk",
            payload.job_id
        );
        Json(SubmitChunkResponse {
            status: "not_found".to_string(),
            updated_worker_total: 0,
        })
    }
}

/////////////////////////////////////
// available_job => skip paused/error/finished
/////////////////////////////////////
async fn available_job(State(state): State<AppState>) -> Json<AvailableJobResponse> {
    let jobs = state.jobs.lock().unwrap();
    let queue = state.run_queue.lock().unwrap();
    let mut counter = state.round_counter.lock().unwrap();

    let in_progress: Vec<String> = queue
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
// assign_chunk => full per-worker approach
/////////////////////////////////////
async fn assign_chunk(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
    Query(query): Query<AssignChunkQuery>,
) -> Result<Json<AssignChunkResponse>, (StatusCode, &'static str)> {
    let now = current_timestamp();

    let mut jobs = state.jobs.lock().unwrap();
    let mut queue = state.run_queue.lock().unwrap();
    let worker_id = query.worker_id.clone();

    if let Some(job) = jobs.get_mut(&job_id) {
        // inactivity => only webapp polls matter => last_webapp_poll
        let elapsed = now.saturating_sub(job.last_webapp_poll);
        if job.status == "in-progress" && elapsed > 10 {
            job.status = "paused".to_string();
            println!(
                "[Scheduler] job_id={} => immediate pause after {}s no webapp poll",
                job_id, elapsed
            );
            remove_from_queue(&mut queue, &job_id);
            return Ok(Json(AssignChunkResponse {
                chunk_index: None,
                chunk_size: 0,
                job_status: "paused".to_string(),
            }));
        }

        match job.status.as_str() {
            "finished" => {
                return Ok(Json(AssignChunkResponse {
                    chunk_index: None,
                    chunk_size: 0,
                    job_status: "finished".to_string(),
                }));
            }
            "error" => {
                return Ok(Json(AssignChunkResponse {
                    chunk_index: None,
                    chunk_size: 0,
                    job_status: "error".to_string(),
                }));
            }
            "paused" => {
                return Ok(Json(AssignChunkResponse {
                    chunk_index: None,
                    chunk_size: 0,
                    job_status: "paused".to_string(),
                }));
            }
            _ => {}
        }

        if job.points_total >= job.points {
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
                worker_id: worker_id.clone(),
                assigned_at: now,
            },
        );

        // figure out how many points remain
        let points_remaining = job.points.saturating_sub(job.points_total);

        // fetch the requesting worker's stats
        let workers_map = state.workers.lock().unwrap();
        let maybe_stats = workers_map.get(&worker_id);

        // default chunk to job's base
        let mut dynamic_chunk = job.chunk_size;

        if let Some(w) = maybe_stats {
            // if worker has some throughput data, tailor chunk just for them
            if w.avg_points_per_sec > 0.0 {
                let target_time = 2.0; // ~2 seconds
                let predicted_chunk = (w.avg_points_per_sec * target_time) as u64;

                // clamp around base chunk size
                if predicted_chunk < job.chunk_size / 2 {
                    dynamic_chunk = job.chunk_size / 2;
                } else if predicted_chunk > job.chunk_size * 2 {
                    dynamic_chunk = job.chunk_size * 2;
                } else {
                    dynamic_chunk = predicted_chunk;
                }
            }
        }

        // do not overshoot remainder
        if dynamic_chunk > points_remaining {
            dynamic_chunk = points_remaining;
        }

        // shrink near the end
        if job.percent_complete > 90.0 {
            let leftover = points_remaining;
            if leftover < dynamic_chunk {
                dynamic_chunk = leftover;
            }
        }

        if dynamic_chunk < 1 {
            dynamic_chunk = 1;
        }

        println!(
            "[Scheduler] job_id={} => worker_id={} => chunk_index={} => chunk_size={}",
            job_id, worker_id, cindex, dynamic_chunk
        );

        Ok(Json(AssignChunkResponse {
            chunk_index: Some(cindex),
            chunk_size: dynamic_chunk,
            job_status: job.status.clone(),
        }))
    } else {
        Err((StatusCode::NOT_FOUND, "Job not found"))
    }
}

/////////////////////////////////////
// mark_job_error
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
            println!("[Scheduler] job_id={} => forcibly error", job_id);

            Json(MarkErrorResponse {
                job_id,
                status: "error".to_string(),
            })
        }
        None => {
            let now = current_timestamp();
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
                created_at: now,
                next_chunk: 0,
                average_chunk_time: 5.0,
                chunks_in_progress: HashMap::new(),
                last_webapp_poll: now,
                samples: Vec::new(),
                killed_chunks: HashSet::new(),
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
// get_job_history
/////////////////////////////////////
async fn get_job_history(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get(&job_id) {
        let resp = HistoryResponse {
            samples: job.samples.clone(),
        };
        (StatusCode::OK, Json(resp))
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(HistoryResponse {
                samples: Vec::new(),
            }),
        )
    }
}

///////////////////////////////////////////////////////////////////////////
// Cleanup => 10s => paused, 300s => error
///////////////////////////////////////////////////////////////////////////
async fn cleanup_inactive_jobs(
    jobs: Arc<Mutex<HashMap<String, JobInfo>>>,
    run_queue: Arc<Mutex<Vec<String>>>,
) {
    const PAUSE_THRESHOLD: u64 = 10;
    const DELETE_THRESHOLD: u64 = 300;

    loop {
        {
            let mut map = jobs.lock().unwrap();
            let mut queue = run_queue.lock().unwrap();
            let now = current_timestamp();

            for (job_id, job) in map.iter_mut() {
                match job.status.as_str() {
                    "in-progress" => {
                        let elapsed = now.saturating_sub(job.last_webapp_poll);
                        if elapsed > PAUSE_THRESHOLD && elapsed < DELETE_THRESHOLD {
                            job.status = "paused".to_string();
                            println!(
                                "[Scheduler] job_id={} => paused after {}s (cleanup, no webapp poll)",
                                job_id, elapsed
                            );
                            remove_from_queue(&mut queue, job_id);
                            continue;
                        } else if elapsed >= DELETE_THRESHOLD {
                            job.status = "error".to_string();
                            println!(
                                "[Scheduler] job_id={} => canceled after 300s no webapp poll",
                                job_id
                            );
                            remove_from_queue(&mut queue, job_id);

                            job.result.clear();
                            job.partial_result.clear();
                            job.percent_complete = 0.0;
                            job.points_in_circle = 0;
                            job.points_total = 0;
                            continue;
                        }
                        chunk_reassign(job, job_id, now);
                    }
                    "paused" => {
                        let elapsed = now.saturating_sub(job.last_webapp_poll);
                        if elapsed >= DELETE_THRESHOLD {
                            job.status = "error".to_string();
                            println!(
                                "[Scheduler] job_id={} => canceled after 300s paused",
                                job_id
                            );
                            remove_from_queue(&mut queue, job_id);

                            job.result.clear();
                            job.partial_result.clear();
                            job.percent_complete = 0.0;
                            job.points_in_circle = 0;
                            job.points_total = 0;
                        }
                    }
                    "finished" | "error" => {}
                    _ => {}
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

///////////////////////////////////////////////////////////////////////////
// chunk_reassign => kill & reassign chunks that have run too long
///////////////////////////////////////////////////////////////////////////
fn chunk_reassign(job: &mut JobInfo, job_id: &str, now: u64) {
    let time_limit = 20.0 * job.average_chunk_time;
    let mut reassign_list = vec![];
    for (&cidx, assignment) in job.chunks_in_progress.iter() {
        let elapsed_sec = now.saturating_sub(assignment.assigned_at) as f64;
        if elapsed_sec > time_limit {
            println!(
                "[Scheduler] job_id={} => chunk={} overdue ({}s > 20×{}s). kill & reassign",
                job_id, cidx, elapsed_sec, job.average_chunk_time
            );
            reassign_list.push(cidx);
        }
    }
    for cidx in reassign_list {
        job.chunks_in_progress.remove(&cidx);
        job.killed_chunks.insert(cidx);
        if cidx < job.next_chunk {
            job.next_chunk = cidx;
        }
    }
}

///////////////////////////////////////////////////////////////////////////
// partial pi sampling => every 500ms
///////////////////////////////////////////////////////////////////////////
async fn sampling_task(jobs: Arc<Mutex<HashMap<String, JobInfo>>>) {
    loop {
        {
            let mut map = jobs.lock().unwrap();
            for (_jid, job) in map.iter_mut() {
                if job.status == "in-progress" && job.points_total > 0 {
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
// Helpers
///////////////////////////////////////////////////////////////////////////
fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn remove_from_queue(queue: &mut Vec<String>, job_id: &str) {
    if let Some(pos) = queue.iter().position(|x| x == job_id) {
        queue.remove(pos);
    }
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
