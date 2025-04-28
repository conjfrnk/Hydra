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
use serde::{Deserialize, Serialize};
use std::env;
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;

/////////////////////////////////////
// AppState, Data (Unchanged)
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
    task_type: String, // "calculate_pi" or "calculate_mandelbrot"
    status: String,    // "in-progress", "paused", "finished", "error"
    result: String,
    partial_result: String,
    percent_complete: f32,

    points: u64,
    chunk_size: u64,
    completed_chunks: u64,
    total_chunks: u64,
    points_in_circle: u64,
    points_total: u64,

    resolution: u32,
    mandel_pixels: HashMap<u64, String>,

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

#[derive(Debug, Serialize, Deserialize, Clone)]
struct SamplePoint {
    percent: f32,
    approx_pi: f64,
}

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    job_id: String,
    task_type: String,
    points: u64,
    #[serde(default)]
    resolution: u32,
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
    points_in_circle: Option<u64>,
    chunk_points: u64,
    mandelbrot_colors: Option<Vec<MandelPixel>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct MandelPixel {
    index: u64,
    color: String,
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
    worker_id: String,
}

#[derive(Debug, Serialize)]
struct AssignChunkResponse {
    chunk_index: Option<u64>,
    chunk_size: u64,
    job_status: String,
    resolution: u32,
    task_type: String,
}

#[derive(Debug, Serialize)]
struct MarkErrorResponse {
    job_id: String,
    status: String,
}

#[derive(Debug, Serialize)]
struct HistoryResponse {
    samples: Vec<SamplePoint>,
    pixels: Vec<MandelPixel>,
}

/////////////////////////////////////
// main
/////////////////////////////////////
#[tokio::main]
async fn main() -> Result<()> {
    let env = env::var("HYDRA_ENV").unwrap_or_else(|_| "development".to_string());
    println!("[Scheduler] Running in HYDRA_ENV={}", env);

    let state = AppState {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        workers: Arc::new(Mutex::new(HashMap::new())),
        run_queue: Arc::new(Mutex::new(Vec::new())),
        round_counter: Arc::new(Mutex::new(0)),
    };

    // periodic tasks
    {
        let sampling_jobs = state.jobs.clone();
        tokio::spawn(async move {
            sampling_task(sampling_jobs).await;
        });
    }
    {
        let jobs_for_cleanup = state.jobs.clone();
        let queue_for_cleanup = state.run_queue.clone();
        tokio::spawn(async move {
            cleanup_inactive_jobs(jobs_for_cleanup, queue_for_cleanup).await;
        });
    }

    // Build Axum app
    let app = Router::new()
        .route("/", get(health_ok))
        .route("/api/create_job", post(create_job))
        .route("/api/job_status/:job_id", get(get_job_status))
        .route("/api/register_worker", post(register_worker))
        .route("/api/submit_chunk", post(submit_chunk))
        .route("/api/available_job", get(available_job))
        .route("/api/assign_chunk/:job_id", get(assign_chunk))
        .route("/api/mark_job_error/:job_id", post(mark_job_error))
        .route("/api/job_history/:job_id", get(get_job_history))
        .with_state(state);

    // In PRODUCTION, we bind to 0.0.0.0:8443 (plaintext) so the ALB can connect
    // In DEV, we might just bind to 127.0.0.1:8443
    let addr: SocketAddr = if env.to_lowercase() == "production" {
        SocketAddr::from(([0, 0, 0, 0], 8443))
    } else {
        SocketAddr::from(([127, 0, 0, 1], 8443))
    };

    println!("[Scheduler] Binding to {}", addr);

    // Plain HTTP with a manual accept loop
    let listener = TcpListener::bind(addr).await?;
    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let app_clone = app.clone();

        tokio::spawn(async move {
            println!("[Scheduler] Accepted connection from {:?}", remote_addr);
            if let Err(err) = Http::new()
                .http1_only(true)
                .serve_connection(stream, app_clone)
                .await
            {
                eprintln!("[Scheduler] server error: {:?}", err);
            }
        });
    }
}

async fn create_job(
    State(state): State<AppState>,
    Json(payload): Json<CreateJobRequest>,
) -> Json<CreateJobResponse> {
    println!("[Scheduler] create_job: {:?}", payload);
    let mut jobs = state.jobs.lock().unwrap();
    let mut queue = state.run_queue.lock().unwrap();

    let now = current_timestamp();
    let base_chunk_size = 100_000;
    let mut total_chunks = (payload.points + base_chunk_size - 1) / base_chunk_size;

    let mut chunk_size = base_chunk_size;
    let mut resolution = payload.resolution;
    let mandel_pixels = HashMap::new();

    if payload.task_type == "calculate_mandelbrot" {
        if resolution < 1 {
            resolution = 256;
        }
        chunk_size = resolution as u64;
        total_chunks = resolution as u64;
    }

    let job_info = JobInfo {
        job_id: payload.job_id.clone(),
        task_type: payload.task_type.clone(),
        status: "in-progress".to_string(),
        result: "".to_string(),
        partial_result: "".to_string(),
        percent_complete: 0.0,

        points: payload.points, // For Mandelbrot, webapp already sends resolution*resolution
        chunk_size,
        completed_chunks: 0,
        total_chunks,
        points_in_circle: 0,
        points_total: 0,
        resolution,
        mandel_pixels,

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
        "[Scheduler] Created job_id={} task_type={} total_chunks={}",
        payload.job_id, payload.task_type, total_chunks
    );

    Json(CreateJobResponse {
        job_id: payload.job_id,
        status: "created".to_string(),
    })
}

async fn get_job_status(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let mut jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get_mut(&job_id) {
        job.last_webapp_poll = current_timestamp();
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
        if job.task_type == "calculate_pi" {
            let points_in_circle = payload.points_in_circle.unwrap_or(0);
            job.completed_chunks += 1;
            job.points_in_circle += points_in_circle;
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
                    "[Scheduler] job_id={} => FINISHED (Pi), final pi={}",
                    payload.job_id, job.result
                );
                remove_from_queue(&mut queue, &payload.job_id);
            } else {
                println!(
                    "[Scheduler] job_id={} => partial pi={}, {}% done",
                    payload.job_id, job.partial_result, job.percent_complete
                );
            }
        } else if job.task_type == "calculate_mandelbrot" {
            job.completed_chunks += 1;
            let row_points = payload.chunk_points;
            job.points_total += row_points;
            if let Some(colors) = &payload.mandelbrot_colors {
                for px in colors {
                    job.mandel_pixels.insert(px.index, px.color.clone());
                }
            }
            job.percent_complete = (job.points_total as f32 / job.points as f32) * 100.0;
            // Instead of filling missing rows with black, we check if all pixels have been computed.
            let total_pixels = (job.resolution as u64) * (job.resolution as u64);
            if job.mandel_pixels.len() as u64 == total_pixels {
                job.status = "finished".to_string();
                job.result = "Mandelbrot complete".to_string();
                job.partial_result = "Done".to_string();
                job.percent_complete = 100.0;
                println!(
                    "[Scheduler] job_id={} => FINISHED (Mandelbrot)",
                    payload.job_id
                );
                remove_from_queue(&mut queue, &payload.job_id);
            } else {
                println!(
                    "[Scheduler] job_id={} => chunk {} done, {}% done",
                    payload.job_id, payload.chunk_index, job.percent_complete
                );
                // Note: Missing rows remain unrendered so they can be reassigned via chunk_reassign.
            }
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
        // Removed heartbeat check so job is not paused based on webapp poll
        if job.points_total >= job.points {
            job.status = "finished".to_string();
            job.result = job.partial_result.clone();
            job.percent_complete = 100.0;
            remove_from_queue(&mut queue, &job_id);
            return Ok(Json(AssignChunkResponse {
                chunk_index: None,
                chunk_size: 0,
                job_status: "finished".to_string(),
                resolution: job.resolution,
                task_type: job.task_type.clone(),
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
        let points_remaining = job.points.saturating_sub(job.points_total);
        let mut dynamic_chunk = job.chunk_size;
        let workers_map = state.workers.lock().unwrap();
        let maybe_stats = workers_map.get(&worker_id);
        if job.task_type == "calculate_pi" {
            if let Some(w) = maybe_stats {
                if w.avg_points_per_sec > 0.0 {
                    let target_time = 2.0;
                    let predicted_chunk = (w.avg_points_per_sec * target_time) as u64;
                    if predicted_chunk < job.chunk_size / 2 {
                        dynamic_chunk = job.chunk_size / 2;
                    } else if predicted_chunk > job.chunk_size * 2 {
                        dynamic_chunk = job.chunk_size * 2;
                    } else {
                        dynamic_chunk = predicted_chunk;
                    }
                }
            }
            if dynamic_chunk > points_remaining {
                dynamic_chunk = points_remaining;
            }
            if dynamic_chunk < 1 {
                dynamic_chunk = 1;
            }
        } else if job.task_type == "calculate_mandelbrot" {
            dynamic_chunk = job.resolution as u64;
        }
        println!(
            "[Scheduler] job_id={} => worker_id={} => chunk_index={} => chunk_size={}",
            job_id, worker_id, cindex, dynamic_chunk
        );
        Ok(Json(AssignChunkResponse {
            chunk_index: Some(cindex),
            chunk_size: dynamic_chunk,
            job_status: job.status.clone(),
            resolution: job.resolution,
            task_type: job.task_type.clone(),
        }))
    } else {
        Err((StatusCode::NOT_FOUND, "Job not found"))
    }
}

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
                task_type: "unknown".to_string(),
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
                resolution: 0,
                mandel_pixels: HashMap::new(),
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

async fn get_job_history(
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> impl IntoResponse {
    let jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get(&job_id) {
        if job.task_type == "calculate_pi" {
            let resp = HistoryResponse {
                samples: job.samples.clone(),
                pixels: vec![],
            };
            (StatusCode::OK, Json(resp))
        } else if job.task_type == "calculate_mandelbrot" {
            let mut all_pixels: Vec<MandelPixel> = job
                .mandel_pixels
                .iter()
                .map(|(idx, col)| MandelPixel {
                    index: *idx,
                    color: col.clone(),
                })
                .collect();
            all_pixels.sort_by_key(|p| p.index);
            let resp = HistoryResponse {
                samples: vec![],
                pixels: all_pixels,
            };
            (StatusCode::OK, Json(resp))
        } else {
            let resp = HistoryResponse {
                samples: vec![],
                pixels: vec![],
            };
            (StatusCode::OK, Json(resp))
        }
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(HistoryResponse {
                samples: Vec::new(),
                pixels: Vec::new(),
            }),
        )
    }
}

async fn cleanup_inactive_jobs(
    jobs: Arc<Mutex<HashMap<String, JobInfo>>>,
    _run_queue: Arc<Mutex<Vec<String>>>,
) {
    loop {
        {
            let mut map = jobs.lock().unwrap();
            let now = current_timestamp();
            for (job_id, job) in map.iter_mut() {
                if job.status == "in-progress" {
                    chunk_reassign(job, job_id, now);
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn chunk_reassign(job: &mut JobInfo, job_id: &str, now: u64) {
    let time_limit = if job.task_type == "calculate_mandelbrot" {
        // For Mandelbrot, rows should be fast; use a fixed short timeout (e.g. 5 seconds)
        5.0
    } else {
        // For Pi jobs, use a multiplier of 40 times the average chunk time.
        40.0 * job.average_chunk_time
    };
    let mut reassign_list = vec![];
    for (&cidx, assignment) in job.chunks_in_progress.iter() {
        let elapsed_sec = now.saturating_sub(assignment.assigned_at) as f64;
        if elapsed_sec > time_limit {
            println!(
                "[Scheduler] job_id={} => chunk={} overdue ({}s > {}s). Reassigning.",
                job_id, cidx, elapsed_sec, time_limit
            );
            reassign_list.push(cidx);
        }
    }
    for cidx in reassign_list {
        job.chunks_in_progress.remove(&cidx);
        if job.task_type == "calculate_mandelbrot" {
            // For Mandelbrot, do not mark as killed; reassign this row by
            // setting next_chunk to the lower index if needed.
            if cidx < job.next_chunk {
                job.next_chunk = cidx;
            }
        } else {
            // For other tasks, mark the chunk as killed and reassign.
            job.killed_chunks.insert(cidx);
            if cidx < job.next_chunk {
                job.next_chunk = cidx;
            }
        }
    }
}

async fn sampling_task(jobs: Arc<Mutex<HashMap<String, JobInfo>>>) {
    loop {
        {
            let mut map = jobs.lock().unwrap();
            for (_jid, job) in map.iter_mut() {
                if job.status == "in-progress"
                    && job.task_type == "calculate_pi"
                    && job.points_total > 0
                {
                    let pi_val = job.partial_result.parse::<f64>().unwrap_or(0.0);
                    let pct = job.percent_complete;
                    if job.samples.is_empty()
                        || (pct as u32) > (job.samples.last().unwrap().percent as u32)
                    {
                        job.samples.push(SamplePoint {
                            approx_pi: pi_val,
                            percent: pct,
                        });
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

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

async fn health_ok() -> impl IntoResponse {
    (StatusCode::OK, "OK\n")
}
