//! Hydra - Worker main entry point
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

use anyhow::{Result, bail};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{env, thread, time::Duration};
use uuid::Uuid;

mod confidential;
use confidential::{ConfidentialCompute, ConfidentialConfig};

////////////////////////////////////////////////
// Data Structures (unchanged)
////////////////////////////////////////////////
#[derive(Debug, Serialize, Deserialize)]
struct AvailableJobResponse {
    job_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AssignChunkResponse {
    chunk_index: Option<u64>,
    chunk_size: u64,
    job_status: String,
    resolution: u32,
    task_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct MandelPixel {
    index: u64,
    color: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct SubmitChunkRequest {
    worker_id: String,
    job_id: String,
    chunk_index: u64,
    points_in_circle: Option<u64>,
    chunk_points: u64,
    mandelbrot_colors: Option<Vec<MandelPixel>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SubmitChunkResponse {
    status: String,
    updated_worker_total: u64,
}

#[derive(Debug, Serialize)]
struct RegisterWorkerRequest {
    worker_id: String,
}

#[derive(Debug, Deserialize)]
struct RegisterWorkerResponse {
    status: String,
    assigned_worker_id: String,
}

////////////////////////////////////////////////
// Constants
////////////////////////////////////////////////

/// The default production Scheduler URL.
const PROD_SCHEDULER_URL: &str = "https://scheduler.hydracompute.com";

/// An optional local URL for development if `--local` is specified.
const LOCAL_SCHEDULER_URL: &str = "http://127.0.0.1:8443";

////////////////////////////////////////////////
// main
////////////////////////////////////////////////
#[tokio::main]
async fn main() -> Result<()> {
    // Parse command-line args to see if user specified --local
    let args: Vec<String> = env::args().collect();
    // Default to production scheduler
    let mut scheduler_url = PROD_SCHEDULER_URL.to_string();

    if args.contains(&"--local".to_string()) {
        scheduler_url = LOCAL_SCHEDULER_URL.to_string();
        println!("[Worker] Running in local mode => {}", scheduler_url);
    } else {
        println!("[Worker] Running in production mode => {}", scheduler_url);
    }

    // Build the HTTP/TLS client
    let client = build_https_client(&scheduler_url).await?;

    // Initialize confidential compute if enabled
    let confidential_config = ConfidentialConfig {
        enabled: env::var("HYDRA_CONFIDENTIAL").unwrap_or_else(|_| "false".to_string()) == "true",
        enclave_path: env::var("HYDRA_ENCLAVE_PATH").ok(),
        attestation_required: env::var("HYDRA_ATTESTATION").unwrap_or_else(|_| "false".to_string()) == "true",
        encryption_enabled: env::var("HYDRA_ENCRYPTION").unwrap_or_else(|_| "true".to_string()) == "true",
    };
    let confidential_config_print = confidential_config.clone();
    let confidential_compute = ConfidentialCompute::new(confidential_config)?;
    println!("[Worker] Confidential compute: enabled={}", confidential_config_print.enabled);

    // Create a unique worker ID
    let worker_id = format!("worker-{}", Uuid::new_v4());

    // Register the Worker in a loop
    loop {
        match register_worker(&client, &worker_id, &scheduler_url).await {
            Ok(_) => break,
            Err(e) => {
                eprintln!("[Worker] register_worker error: {:?}", e);
                eprintln!("[Worker] Retrying in 2 seconds...");
                thread::sleep(Duration::from_secs(2));
            }
        }
    }

    // Infinity loop: pick an available job, do a chunk, submit. Repeat.
    loop {
        match find_available_job(&client, &scheduler_url).await {
            Ok(Some(job_id)) => {
                let chunk_data =
                    match assign_chunk(&client, &job_id, &worker_id, &scheduler_url).await {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("[Worker] assign_chunk error: {:?}", e);
                            thread::sleep(Duration::from_secs(2));
                            continue;
                        }
                    };

                if chunk_data.chunk_index.is_none() {
                    // job finished/error/paused => no chunk
                    println!("[Worker] job_id={} => no chunk => done/err/paused?", job_id);
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }

                let cindex = chunk_data.chunk_index.unwrap();
                let cpoints = chunk_data.chunk_size;
                println!("[Worker] job_id={} => working on chunk {}", job_id, cindex);

                if chunk_data.task_type == "calculate_pi" {
                    let chunk_data_bytes = cpoints.to_le_bytes().to_vec();
                    let result = confidential_compute.compute_in_enclave("calculate_pi", &chunk_data_bytes).await?;
                    let points_in_circle = u64::from_le_bytes(result[..8].try_into().unwrap_or([0; 8]));
                    
                    let sc_req = SubmitChunkRequest {
                        worker_id: worker_id.clone(),
                        job_id: job_id.clone(),
                        chunk_index: cindex,
                        points_in_circle: Some(points_in_circle),
                        chunk_points: cpoints,
                        mandelbrot_colors: None,
                    };
                    if let Err(e) = submit_chunk(&client, &sc_req, &scheduler_url).await {
                        eprintln!("[Worker] submit_chunk error: {:?}", e);
                    }
                } else if chunk_data.task_type == "calculate_mandelbrot" {
                    let row_index = cindex as u32;
                    let resolution = chunk_data.resolution;
                    
                    // Prepare chunk data for confidential compute
                    let mut chunk_data_bytes = Vec::new();
                    chunk_data_bytes.extend_from_slice(&row_index.to_le_bytes());
                    chunk_data_bytes.extend_from_slice(&resolution.to_le_bytes());
                    
                    let result = confidential_compute.compute_in_enclave("calculate_mandelbrot", &chunk_data_bytes).await?;
                    let row_colors: Vec<MandelPixel> = serde_json::from_slice(&result).unwrap_or_default();

                    let sc_req = SubmitChunkRequest {
                        worker_id: worker_id.clone(),
                        job_id: job_id.clone(),
                        chunk_index: cindex,
                        points_in_circle: None,
                        chunk_points: cpoints,
                        mandelbrot_colors: Some(row_colors),
                    };
                    if let Err(e) = submit_chunk(&client, &sc_req, &scheduler_url).await {
                        eprintln!("[Worker] submit_chunk error: {:?}", e);
                    }
                } else {
                    println!("[Worker] Unknown task_type={}", chunk_data.task_type);
                }

                thread::sleep(Duration::from_millis(200));
            }
            Ok(None) => {
                // No job => sleep a bit
                println!("[Worker] No in-progress jobs => sleeping...");
                thread::sleep(Duration::from_secs(2));
            }
            Err(e) => {
                eprintln!("[Worker] find_available_job error: {:?}", e);
                thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

////////////////////////////////////////////////
// Helper Functions
////////////////////////////////////////////////

/// Build an HTTPS client. If local mode, we might skip cert verification.
async fn build_https_client(scheduler_url: &str) -> Result<Client> {
    // We'll check if the user is pointing to localhost or 127.0.0.1. If so, let's allow invalid certs.
    let is_local = scheduler_url.contains("127.0.0.1");

    let mut builder = Client::builder().use_rustls_tls();
    if is_local {
        println!("[Worker] Local dev => accepting invalid TLS certs");
        builder = builder.danger_accept_invalid_certs(true);
    }

    let client = builder.build()?;
    Ok(client)
}

async fn register_worker(client: &Client, worker_id: &str, scheduler_url: &str) -> Result<()> {
    let url = format!("{}/api/register_worker", scheduler_url);
    let req_body = RegisterWorkerRequest {
        worker_id: worker_id.to_string(),
    };
    let resp = client.post(&url).json(&req_body).send().await?;
    if !resp.status().is_success() {
        bail!(
            "register_worker => status={} body={}",
            resp.status(),
            resp.text().await?
        );
    }
    let parsed: RegisterWorkerResponse = resp.json().await?;
    println!(
        "[Worker] Registered => status={}, assigned_worker_id={}",
        parsed.status, parsed.assigned_worker_id
    );
    Ok(())
}

async fn find_available_job(client: &Client, scheduler_url: &str) -> Result<Option<String>> {
    let url = format!("{}/api/available_job", scheduler_url);
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        bail!(
            "find_available_job => status={} body={}",
            resp.status(),
            resp.text().await?
        );
    }
    let avail: AvailableJobResponse = resp.json().await?;
    Ok(avail.job_id)
}

async fn assign_chunk(
    client: &Client,
    job_id: &str,
    worker_id: &str,
    scheduler_url: &str,
) -> Result<AssignChunkResponse> {
    let url = format!(
        "{}/api/assign_chunk/{}?worker_id={}",
        scheduler_url, job_id, worker_id
    );
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        bail!(
            "assign_chunk => status={} body={}",
            resp.status(),
            resp.text().await?
        );
    }
    let chunk_resp: AssignChunkResponse = resp.json().await?;
    Ok(chunk_resp)
}

async fn submit_chunk(
    client: &Client,
    sc_req: &SubmitChunkRequest,
    scheduler_url: &str,
) -> Result<()> {
    let url = format!("{}/api/submit_chunk", scheduler_url);
    let resp = client.post(&url).json(&sc_req).send().await?;
    if !resp.status().is_success() {
        bail!(
            "submit_chunk => status={} body={}",
            resp.status(),
            resp.text().await?
        );
    }
    let sc_data: SubmitChunkResponse = resp.json().await?;
    println!(
        "[Worker] chunk_submitted => status={}, updated_worker_total={}",
        sc_data.status, sc_data.updated_worker_total
    );
    Ok(())
}

fn do_monte_carlo(n: u64) -> u64 {
    let mut count = 0;
    for _ in 0..n {
        let x: f64 = rand::random();
        let y: f64 = rand::random();
        if x * x + y * y <= 1.0 {
            count += 1;
        }
    }
    count
}

fn compute_mandel_row(row_index: u32, resolution: u32) -> Vec<MandelPixel> {
    let mut row_data = Vec::with_capacity(resolution as usize);

    for col_index in 0..resolution {
        let x_frac = col_index as f64 / (resolution as f64 - 1.0);
        let y_frac = row_index as f64 / (resolution as f64 - 1.0);

        let x0 = -2.0 + 3.0 * x_frac;
        let y0 = -1.5 + 3.0 * y_frac;

        let color = mandel_color(x0, y0);
        let pixel_index = (row_index as u64) * (resolution as u64) + (col_index as u64);

        row_data.push(MandelPixel {
            index: pixel_index,
            color,
        });
    }
    row_data
}

fn mandel_color(cx: f64, cy: f64) -> String {
    let max_iter = 300u32;
    let mut x = 0.0;
    let mut y = 0.0;
    let mut iter = 0;

    while x * x + y * y <= 4.0 && iter < max_iter {
        let x_temp = x * x - y * y + cx;
        y = 2.0 * x * y + cy;
        x = x_temp;
        iter += 1;
    }

    if iter >= max_iter {
        "#000000".to_string()
    } else {
        // Simple hue-based coloring
        let hue = (iter as f64 / max_iter as f64) * 360.0;
        hsv_to_rgb_hex(hue, 1.0, 1.0)
    }
}

fn hsv_to_rgb_hex(h: f64, s: f64, v: f64) -> String {
    let c = s * v;
    let hh = h / 60.0;
    let x = c * (1.0 - ((hh % 2.0) - 1.0).abs());
    let (r1, g1, b1) = match hh {
        h if h >= 0.0 && h < 1.0 => (c, x, 0.0),
        h if h >= 1.0 && h < 2.0 => (x, c, 0.0),
        h if h >= 2.0 && h < 3.0 => (0.0, c, x),
        h if h >= 3.0 && h < 4.0 => (0.0, x, c),
        h if h >= 4.0 && h < 5.0 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let r = (r1 + m) * 255.0;
    let g = (g1 + m) * 255.0;
    let b = (b1 + m) * 255.0;

    format!("#{:02X}{:02X}{:02X}", r as u8, g as u8, b as u8)
}
