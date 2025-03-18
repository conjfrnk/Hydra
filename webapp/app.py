import os
import uuid
import requests
from flask import Flask, request, jsonify, render_template

app = Flask(__name__)

SCHEDULER_URL = "https://127.0.0.1:8443"
SCHEDULER_CERT = os.path.join("certs", "scheduler_cert.pem")

job_data = {}

@app.route("/")
def index():
    return render_template("index.html")

@app.route("/mark_old_job", methods=["POST"])
def mark_old_job():
    """
    Kills exactly one old job. The front-end calls this if its `currentJobId` is in progress,
    so we forcibly mark it as error on the Scheduler side.
    """
    data = request.get_json()
    if not data or "old_job_id" not in data:
        return jsonify({"error": "No 'old_job_id' provided"}), 400

    old_job_id = data["old_job_id"]
    try:
        mark_url = f"{SCHEDULER_URL}/api/mark_job_error/{old_job_id}"
        resp = requests.post(mark_url, verify=SCHEDULER_CERT)
        print(f"[WebApp] Mark job={old_job_id} => status_code={resp.status_code}")
    except Exception as e:
        print(f"[WebApp] Error marking job={old_job_id} as error: {e}")

    # Locally mark it as error
    if old_job_id in job_data:
        job_data[old_job_id]["status"] = "error"

    return jsonify({"status": "old job killed"}), 200

@app.route("/start_job", methods=["POST"])
def start_job():
    """
    Creates a brand-new job on the scheduler.
    The front-end calls /mark_old_job first if needed.
    """
    data = request.get_json()
    if not data or "points" not in data:
        return jsonify({"error": "Missing 'points'"}), 400

    points = data["points"]
    job_id = str(uuid.uuid4())

    job_data[job_id] = {
        "status": "in-progress",
        "result": "",
        "partial_result": "",
        "percent_complete": 0.0
    }

    try:
        create_url = f"{SCHEDULER_URL}/api/create_job"
        resp = requests.post(
            create_url,
            json={
                "job_id": job_id,
                "task_type": "calculate_pi",
                "points": int(points)
            },
            verify=SCHEDULER_CERT
        )
        print(f"[WebApp] Scheduler create_job => status_code={resp.status_code}")
        resp.raise_for_status()
    except Exception as e:
        print(f"[WebApp] Exception contacting Scheduler: {e}")
        return jsonify({"error": str(e)}), 500

    return jsonify({"job_id": job_id}), 200

@app.route("/job_status/<job_id>", methods=["GET"])
def job_status(job_id):
    """
    Poll the scheduler for the latest status and store it locally.
    """
    if job_id not in job_data:
        return jsonify({"error": "Unknown job_id"}), 404

    try:
        resp = requests.get(
            f"{SCHEDULER_URL}/api/job_status/{job_id}",
            verify=SCHEDULER_CERT
        )
        if resp.ok:
            sched_info = resp.json()
            job_data[job_id]["status"] = sched_info["status"]
            job_data[job_id]["result"] = sched_info["result"]
            job_data[job_id]["partial_result"] = sched_info["partial_result"]
            job_data[job_id]["percent_complete"] = sched_info["percent_complete"]
        else:
            print(f"[WebApp] Non-OK response from Scheduler for {job_id}: {resp.text}")
    except Exception as e:
        print(f"[WebApp] Exception in job_status for {job_id}: {e}")

    return jsonify(job_data[job_id])

@app.route("/job_history/<job_id>", methods=["GET"])
def job_history(job_id):
    """
    Returns the entire sample list for the given job from the Scheduler
    """
    try:
        url = f"{SCHEDULER_URL}/api/job_history/{job_id}"
        resp = requests.get(url, verify=SCHEDULER_CERT)
        if not resp.ok:
            return jsonify({"error": f"Non-OK from scheduler: {resp.status_code} {resp.text}"}), resp.status_code

        data = resp.json()
        return jsonify(data), 200
    except Exception as e:
        print(f"[WebApp] Exception in job_history: {e}")
        return jsonify({"error": str(e)}), 500

if __name__ == "__main__":
    print("[WebApp] Starting Flask app on http://127.0.0.1:5000")
    app.run(host="127.0.0.1", port=5000, debug=True)
