// File: heartbeatWorker.js
// Responsible for polling /job_status/<job_id> in the background.

let currentJobId = "";
let pollingInterval = null;

self.onmessage = async function(e) {
  const msg = e.data;
  if (msg.type === "START") {
    currentJobId = msg.jobId;
    const intervalMs = msg.intervalMs || 1000; // default 1s
    if (pollingInterval) clearInterval(pollingInterval);
    pollingInterval = setInterval(() => {
      if (currentJobId) pollJobStatus(currentJobId);
    }, intervalMs);
  } else if (msg.type === "STOP") {
    currentJobId = "";
    if (pollingInterval) {
      clearInterval(pollingInterval);
      pollingInterval = null;
    }
  }
};

async function pollJobStatus(jobId) {
  try {
    let resp = await fetch(`/job_status/${jobId}`);
    if (!resp.ok) {
      const text = await resp.text();
      self.postMessage({ type: "ERROR", info: text });
      return;
    }
    let sData = await resp.json();
    self.postMessage({ type: "STATUS", data: sData, jobId });
  } catch(e) {
    self.postMessage({ type: "ERROR", info: e.toString() });
  }
}
