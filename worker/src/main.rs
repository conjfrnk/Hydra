use anyhow::{anyhow, bail, Result};
use rand::Rng;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{thread, time::Duration};

///////////////////////////////////////
// Worker Constants
///////////////////////////////////////
const SCHEDULER_URL: &str = "https://127.0.0.1:8443";
const SCHEDULER_CERT_PATH: &str = "certs/scheduler_cert.pem";

///////////////////////////////////////
// Data Structures
///////////////////////////////////////
#[derive(Debug, Serialize, Deserialize)]
struct AvailableJobResponse {
    job_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AssignChunkResponse {
    chunk_index: Option<u64>,
    chunk_size: u64,
    job_status: String, // in-progress, finished, or error
}

#[derive(Debug, Serialize, Deserialize)]
struct SubmitChunkRequest {
    worker_id: String,
    job_id: String,
    points_in_circle: u64,
    chunk_points: u64,
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

///////////////////////////////////////
// Main Worker
///////////////////////////////////////
#[tokio::main]
async fn main() -> Result<()> {
    // 1) Build an HTTPS client in a loop, so if connection is refused,
    // we just wait and retry (like if the connection was dropped).
    let client = loop {
        match build_https_client().await {
            Ok(c) => break c,
            Err(e) => {
                eprintln!("[Worker] build_https_client error: {:?}", e);
                eprintln!("[Worker] Retrying in 2 seconds...");
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        }
    };

    let worker_id = "my_single_worker".to_string();

    // 2) Register the Worker in a loop, so we never permanently fail if
    // the Scheduler is offline. We'll keep trying until success.
    loop {
        match register_worker(&client, &worker_id).await {
            Ok(_) => break,
            Err(e) => {
                eprintln!("[Worker] register_worker error: {:?}", e);
                eprintln!("[Worker] Retrying in 2 seconds...");
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        }
    }

    // 3) Infinity loop: pick an available job, do a chunk, submit. Then repeat.
    // This allows the Worker to time-slice across multiple jobs if the Scheduler round-robins them.
    loop {
        // A) find a job
        match find_available_job(&client).await {
            Ok(Some(job_id)) => {
                // B) assign a chunk
                let chunk_data = match assign_chunk(&client, &job_id).await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("[Worker] assign_chunk error: {:?}", e);
                        // Sleep and try again
                        thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                };

                if chunk_data.chunk_index.is_none() {
                    // job is finished or error
                    println!(
                        "[Worker] job_id={} => no chunk assigned => done/err",
                        job_id
                    );
                    // Just loop again for another job
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }

                let cindex = chunk_data.chunk_index.unwrap();
                let cpoints = chunk_data.chunk_size;
                println!("[Worker] job_id={} => chunk_index={}", job_id, cindex);

                // C) do local Monte Carlo
                let points_in_circle = do_monte_carlo(cpoints);

                // D) submit
                let sc_req = SubmitChunkRequest {
                    worker_id: worker_id.clone(),
                    job_id: job_id.clone(),
                    points_in_circle,
                    chunk_points: cpoints,
                };
                match submit_chunk(&client, &sc_req).await {
                    Ok(_) => (),
                    Err(e) => {
                        eprintln!("[Worker] submit_chunk error: {:?}", e);
                        // We'll just loop around
                    }
                }

                // short sleep
                thread::sleep(Duration::from_millis(200));
            }
            Ok(None) => {
                // No job => sleep, then check again
                println!("[Worker] No in-progress jobs => sleeping...");
                thread::sleep(Duration::from_secs(2));
            }
            Err(e) => {
                eprintln!("[Worker] find_available_job error: {:?}", e);
                eprintln!("[Worker] Retrying in 2 seconds...");
                thread::sleep(Duration::from_secs(2));
            }
        }
    }
}

///////////////////////////////////////
// Helper Functions
///////////////////////////////////////

// Build an HTTPS reqwest client with the scheduler's certificate
async fn build_https_client() -> Result<Client> {
    let ca = std::fs::read(SCHEDULER_CERT_PATH)
        .map_err(|e| anyhow!("Error reading certificate file: {:?}", e))?;
    let cert = reqwest::Certificate::from_pem(&ca)
        .map_err(|e| anyhow!("Error parsing certificate: {:?}", e))?;
    let client = Client::builder()
        .use_rustls_tls()
        .add_root_certificate(cert)
        .build()?;
    Ok(client)
}

async fn register_worker(client: &Client, worker_id: &str) -> Result<()> {
    let url = format!("{}/api/register_worker", SCHEDULER_URL);
    let req_body = RegisterWorkerRequest {
        worker_id: worker_id.to_string(),
    };
    let resp = client.post(&url).json(&req_body).send().await?;
    if !resp.status().is_success() {
        bail!(
            "register_worker => unexpected status={} body={}",
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

async fn find_available_job(client: &Client) -> Result<Option<String>> {
    let url = format!("{}/api/available_job", SCHEDULER_URL);
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

async fn assign_chunk(client: &Client, job_id: &str) -> Result<AssignChunkResponse> {
    let url = format!("{}/api/assign_chunk/{}", SCHEDULER_URL, job_id);
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

async fn submit_chunk(client: &Client, sc_req: &SubmitChunkRequest) -> Result<()> {
    let url = format!("{}/api/submit_chunk", SCHEDULER_URL);
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
    let mut rng = rand::thread_rng();
    let mut count = 0;
    for _ in 0..n {
        let x: f64 = rng.gen();
        let y: f64 = rng.gen();
        if x * x + y * y <= 1.0 {
            count += 1;
        }
    }
    count
}
