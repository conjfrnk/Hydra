# Hydra — Cloud Computing with Many Heads

Hydra lets idle laptops, desktops, and single-board computers cooperate on embarrassingly-parallel work.  
The current prototype pairs a **Rust** scheduler + worker binary with a **Python (Flask)** web front-end.

> **Project status:** functional PoC  
> – ✅ Dynamic chunking for Monte-Carlo π  
> – ✅ Distributed Mandelbrot rendering (static row chunks)  
> – ✅ Automatic retry / chunk reassignment  
> – ⚠️ No secure enclaves yet • No token rewards yet • No carbon-aware dispatch yet

A longer technical write-up (architecture, benchmarks, carbon analysis) is in `written_final_report.pdf`

---

## Live demo

<https://hydracompute.com> – runs the same code in this repo behind an ALB with a public TLS cert.

---

## Quick architecture glance
* **Scheduler** (`scheduler/`): keeps per-job state, sizes chunks to ~2 s per worker, reassigns slow or lost chunks.  
* **Worker** (`worker/`): polls for work, runs either π simulation or Mandelbrot row, submits results.  
* **WebApp** (`webapp/`): simple UI + polling; shows progress and renders images/charts in real-time.

---

## Feature highlights

| Area | What works today | Notes |
|------|------------------|-------|
| Monte-Carlo π | Adaptive chunk sizes per worker | ~8 × speed-up on 8 heterogeneous nodes |
| Mandelbrot | One row = one chunk | Dynamic chunking planned |
| Fault tolerance | Over-due chunks auto-requeued; workers can disappear | Parameter = 40 × average chunk time |
| Transport security | TLS by default (skip verification in local dev) | No SGX/SEV yet – **don’t send secrets** |
| Deployment | Cargo binaries or multi-stage Dockerfiles | See below |

---

## Local quick-start

### 1 · Prerequisites

* **Rust 1.77+** (install with `rustup`)
* **Python 3.10+** (for Flask UI)
* Self-signed cert/key in `certs/` (only needed for local HTTPS)

### 2 · Run the scheduler
```bash
cd scheduler
cargo run --release
# listens on https://127.0.0.1:8443
```

### 3 · Launch one or more workers
```bash
cd worker
cargo run --release -- --local     # --local → trust the self-signed cert
# open new terminals for extra workers
```

### 4 · Start the web front-end
```bash
cd webapp
python app.py           # http://127.0.0.1:5000
```

## Docker quick-start (optional)
```bash
# scheduler
docker build -t hydra-scheduler -f scheduler/Dockerfile .
docker run -p 8443:8443 hydra-scheduler

# worker
docker build -t hydra-worker -f worker/Dockerfile .
docker run hydra-worker --local     # repeat for more instances

# webapp (uses system Python image)
cd webapp
docker build -t hydra-webapp .
docker run -p 5000:5000 hydra-webapp
```

## Roadmap (next 12 months)
* **Confidential compute:** SGX/SEV support so sensitive data can run on untrusted volunteers
* **Grid-aware scheduler:** shift chunks toward regions with cleaner electricity
* **Adaptive Mandelbrot tiling:** variable-height stripes to remove current load imbalance
* **Tokenised incentives:** optional micro-payments to offset volunteer electricity
