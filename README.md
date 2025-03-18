# Hydra

## Distributed Pi Calculation

This part of the project demonstrates a **distributed approach to calculating $\pi$ (pi)** using **dynamic chunking**. It involves three main components:

1. **Scheduler (Rust)** – Coordinates jobs, assigns work chunks, and tracks completion.  
2. **Worker (Rust)** – Performs Monte Carlo simulations for a given chunk of points.  
3. **WebApp (Python Flask)** – Provides a front-end for users to create Pi calculation jobs and view progress/results in real time.

---

## Overview

### General Flow

- The user visits the **WebApp**, inputs a large number of random points (e.g., 1 billion).
- The WebApp instructs the **Scheduler** to create a new job.
- **Workers** (which can be on the same machine or multiple machines) query the Scheduler for an available job, receive a chunk of random points to compute, run the Monte Carlo step, and submit partial results back to the Scheduler.
- The Scheduler aggregates partial results (points inside the circle vs. total points) to estimate $\pi$.
- The WebApp periodically polls the Scheduler to update the progress and final result in a browser chart.

### Monte Carlo Pi Calculation

- Each Worker randomly generates points $(x, y)$ in the unit square $[0,1) \times [0,1)$.
- A point lies inside the unit circle if $x^2 + y^2 \leq 1$.
- The fraction of points inside vs. total points, multiplied by 4, approximates $\pi$. (Because the area of a unit circle is $\pi$, and the square’s area is 1, so the ratio $\pi / 4$ is expected if points are uniformly distributed.)

### Dynamic Chunking

- Instead of splitting the total points in a static way, the Scheduler assigns multiple, smaller “chunks” of points to each Worker.
- Over time, the Scheduler measures each Worker’s throughput (points computed per second).
- When a Worker requests more work, the Scheduler sizes the chunk based on **that Worker’s** observed performance, aiming for ~2 seconds of compute.
- This ensures faster Workers get larger chunks, slower Workers get smaller chunks, maximizing overall throughput and preventing idle time.

---

## Key Components

### Scheduler

- **Language**: Rust
- **Location**: `scheduler/` directory
- **Responsibilities**:
  1. Maintains a list of jobs. Each job tracks number of points, partial result, percentage complete, and status.
  2. Receives chunk submissions (how many points in the circle vs. total).
  3. Tracks each Worker’s measured speed (`avg_points_per_sec`) so it can adapt chunk sizes.
  4. Cleans up inactive or stalled jobs.
  5. Provides a REST API for the WebApp and Workers.

- **Endpoints** (subset):
  - `POST /api/create_job`: Creates a new job (given number of points, etc.).
  - `GET /api/assign_chunk/:job_id?worker_id=...`: Returns a custom chunk for the worker, based on that worker’s performance.
  - `POST /api/submit_chunk`: Worker submits chunk results.
  - `GET /api/job_status/:job_id`: WebApp queries job progress.

### Worker

- **Language**: Rust
- **Location**: `worker/` directory
- **Responsibilities**:
  1. Registers itself with the Scheduler (`/api/register_worker`).
  2. Repeatedly checks for an available job.
  3. Calls `assign_chunk?worker_id=...` to get a chunk sized for its performance.
  4. Performs the Monte Carlo simulation for that chunk, then `submit_chunk` with results.
  5. Loops indefinitely, requesting new chunks until the job is finished or no jobs remain.

### WebApp

- **Language**: Python (Flask)
- **Location**: `webapp/` directory
- **Responsibilities**:
  1. Renders an HTML page (`index.html`) with a user form to create a new Pi calculation job.
  2. Sends a request to the Scheduler to create a job, then starts a background polling process to track progress.
  3. Displays progress (percent complete) and partial π approximations on a Chart.js graph in real time.
  4. Can kill a previously running job if the user starts a new one.

---

## Running Locally

Follow these steps to run the **Scheduler**, **Worker**, and **WebApp** on the same machine:

1. **Prerequisites**
   - **Rust** (Cargo) installed (for building Scheduler & Worker).
   - **Python 3** and `pip` (for the WebApp).
   - Local TLS certificate and key for the Scheduler (`certs/scheduler_cert.pem`, `certs/scheduler_key.pem`). Copy these to "certs" directories in the scheduler/worker directories.
   - Configure or trust the self-signed certificate so the Worker can connect to `https://127.0.0.1:8443`.

2. **Build & Run the Scheduler**
   ```bash
   cd scheduler
   cargo build --release
   cargo run
   ```
   The Scheduler listens on **port 8443** by default.

3. **Build & Run the Worker**
   ```bash
   cd worker
   cargo build --release
   cargo run
   ```
   The Worker registers itself, then polls the Scheduler for available jobs.
   
4. **Run the WebApp**
   ```bash
   cd webapp
   # (Optional) python -m venv && source venv/bin/activate
   # pip install -r requirements.txt (if used)
   python app.py
   ```
   The Flask server starts on **port 5000** by default
   
5. **Open the Web Interface**
   - Navigate to ```https://127.0.0.1:5000```
   - Enter the desired number of random points and hit **Calculate**
   - Watch the progress bar and chart update as workers submit chunks

---

## Notes

- **Chunk Overhead**: Chunk assignments have communication overhead, so chunk sizing is important. I am still working on this.
- **Scaling**: You can run multiple Workers on different machines, or on the same machine, all talking to the same Scheduler.
