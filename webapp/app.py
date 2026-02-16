# Hydra - Python Flask WebApp
# Copyright (C) 2025 Connor Frank
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program. If not, see <https://www.gnu.org/licenses/>.

import os
import uuid
import requests
from flask import Flask, request, jsonify, render_template

app = Flask(__name__)

# 1) Check environment to decide how to contact the Scheduler.
#    - If HYDRA_ENV=production, we assume there's a real domain with a valid TLS cert (via ALB).
#    - Otherwise (dev/local), default to "http://127.0.0.1:8443" with no SSL verify.
IS_PRODUCTION = os.getenv("HYDRA_ENV", "").lower() == "production"
SCHEDULER_URL = os.getenv("SCHEDULER_URL", "http://127.0.0.1:8443")

# 2) In production, we rely on system CA store. In dev, we skip verifying (in case of self-signed).
VERIFY_HTTPS = True if IS_PRODUCTION else False

# We store local status of each job to avoid spamming the scheduler unnecessarily
job_data = {}


#################################################
# Landing Page
#################################################
@app.route("/")
def home():
    return render_template("index.html")


#################################################
# Pi Subpage
#################################################
@app.route("/pi")
def pi_page():
    return render_template("pi.html")


#################################################
# Mandelbrot Subpage
#################################################
@app.route("/mandelbrot")
def mandelbrot_page():
    return render_template("mandelbrot.html")


#################################################
# Mark Old Job (common)
#################################################
@app.route("/mark_old_job", methods=["POST"])
def mark_old_job():
    data = request.get_json()
    if not data or "old_job_id" not in data:
        return jsonify({"error": "No 'old_job_id' provided"}), 400

    old_job_id = data["old_job_id"]
    try:
        mark_url = f"{SCHEDULER_URL}/api/mark_job_error/{old_job_id}"
        resp = requests.post(mark_url, verify=VERIFY_HTTPS)
        print(f"[WebApp] Mark job={old_job_id} => status_code={resp.status_code}")
    except Exception as e:
        print(f"[WebApp] Error marking job={old_job_id} as error: {e}")

    if old_job_id in job_data:
        job_data[old_job_id]["status"] = "error"

    return jsonify({"status": "old job killed"}), 200


#################################################
# Start Job (common) => Pi or Mandelbrot
#################################################
@app.route("/start_job", methods=["POST"])
def start_job():
    data = request.get_json()
    if not data or "task_type" not in data:
        return jsonify({"error": "Missing 'task_type'"}), 400

    task_type = data["task_type"]
    job_id = str(uuid.uuid4())

    # Create local job record
    job_data[job_id] = {
        "status": "in-progress",
        "result": "",
        "partial_result": "",
        "percent_complete": 0.0,
    }

    # Build request to Scheduler
    payload = {"job_id": job_id, "task_type": task_type}

    if task_type == "calculate_pi":
        if "points" not in data:
            return jsonify({"error": "Missing 'points' for Pi"}), 400
        payload["points"] = int(data["points"])

    elif task_type == "calculate_mandelbrot":
        if "resolution" not in data:
            return jsonify({"error": "Missing 'resolution' for Mandelbrot"}), 400
        resolution = int(data["resolution"])
        payload["points"] = resolution * resolution
        payload["resolution"] = resolution
    else:
        return jsonify({"error": f"Unknown task_type: {task_type}"}), 400

    try:
        create_url = f"{SCHEDULER_URL}/api/create_job"
        resp = requests.post(create_url, json=payload, verify=VERIFY_HTTPS)
        print(f"[WebApp] Scheduler create_job => status_code={resp.status_code}")
        resp.raise_for_status()
    except Exception as e:
        print(f"[WebApp] Exception contacting Scheduler: {e}")
        return jsonify({"error": str(e)}), 500

    return jsonify({"job_id": job_id}), 200


#################################################
# Job Status
#################################################
@app.route("/job_status/<job_id>", methods=["GET"])
def job_status(job_id):
    try:
        resp = requests.get(
            f"{SCHEDULER_URL}/api/job_status/{job_id}", verify=VERIFY_HTTPS
        )
        if resp.ok:
            sched_info = resp.json()
            job_data[job_id] = sched_info  # update our local record with the latest data
        else:
            print(f"[WebApp] Non-OK response from Scheduler: {resp.text}")
    except Exception as e:
        print(f"[WebApp] Exception in job_status for {job_id}: {e}")

    return jsonify(job_data.get(job_id, {"error": "Job not found"}))


#################################################
# Job History => Pi or partial Mandelbrot data
#################################################
@app.route("/job_history/<job_id>", methods=["GET"])
def job_history(job_id):
    try:
        url = f"{SCHEDULER_URL}/api/job_history/{job_id}"
        resp = requests.get(url, verify=VERIFY_HTTPS)
        if not resp.ok:
            return (
                jsonify(
                    {"error": f"Non-OK from scheduler: {resp.status_code} {resp.text}"}
                ),
                resp.status_code,
            )
        data = resp.json()
        return jsonify(data), 200
    except Exception as e:
        print(f"[WebApp] Exception in job_history: {e}")
        return jsonify({"error": str(e)}), 500


@app.route("/api/token_metrics", methods=["GET"])
def token_metrics():
    try:
        url = f"{SCHEDULER_URL}/api/token_metrics"
        resp = requests.get(url, verify=VERIFY_HTTPS)
        if not resp.ok:
            return jsonify({"error": f"Non-OK from scheduler: {resp.status_code}"}), resp.status_code
        return jsonify(resp.json()), 200
    except Exception as e:
        print(f"[WebApp] Exception in token_metrics: {e}")
        return jsonify({"error": str(e)}), 500


@app.route("/api/sustainability_metrics", methods=["GET"])
def sustainability_metrics():
    try:
        url = f"{SCHEDULER_URL}/api/sustainability_metrics"
        resp = requests.get(url, verify=VERIFY_HTTPS)
        if not resp.ok:
            return jsonify({"error": f"Non-OK from scheduler: {resp.status_code}"}), resp.status_code
        return jsonify(resp.json()), 200
    except Exception as e:
        print(f"[WebApp] Exception in sustainability_metrics: {e}")
        return jsonify({"error": str(e)}), 500


if __name__ == "__main__":
    print("[WebApp] Starting Flask app on http://127.0.0.1:5000")
    app.run(host="127.0.0.1", port=5000, debug=True)
