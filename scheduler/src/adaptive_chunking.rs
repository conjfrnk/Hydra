//! Hydra - Adaptive Chunking Module
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

// use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkPerformance {
    pub chunk_id: u64,
    pub worker_id: String,
    pub points_processed: u64,
    pub computation_time: f64, // seconds
    pub points_per_second: f64,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveChunkingConfig {
    pub enabled: bool,
    pub min_chunk_size: u64,
    pub max_chunk_size: u64,
    pub target_chunk_time: f64, // seconds
    pub performance_history_size: usize,
    pub adaptive_scaling_factor: f64,
    pub worker_specific_chunking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptiveChunk {
    pub chunk_id: u64,
    pub start_point: u64,
    pub end_point: u64,
    pub estimated_difficulty: f64,
    pub assigned_worker: Option<String>,
    pub priority: f64,
}

pub struct AdaptiveChunking {
    config: AdaptiveChunkingConfig,
    worker_performance: HashMap<String, WorkerPerformanceProfile>,
    chunk_history: Vec<ChunkPerformance>,
    total_points: u64,
    current_chunk_id: u64,
}

#[derive(Debug, Clone)]
struct WorkerPerformanceProfile {
    worker_id: String,
    avg_points_per_second: f64,
    recent_performance: Vec<f64>,
    last_updated: u64,
    reliability_score: f64, // 0.0 to 1.0
}

impl AdaptiveChunking {
    pub fn new(config: AdaptiveChunkingConfig, total_points: u64) -> Self {
        Self {
            config,
            worker_performance: HashMap::new(),
            chunk_history: Vec::new(),
            total_points,
            current_chunk_id: 0,
        }
    }

    pub fn calculate_optimal_chunk_size(&self, worker_id: &str, remaining_points: u64) -> u64 {
        if !self.config.enabled {
            return self.config.min_chunk_size;
        }

        let worker_profile = self.worker_performance.get(worker_id);
        let base_performance = worker_profile
            .map(|p| p.avg_points_per_second)
            .unwrap_or(1000.0); // Default performance

        // Calculate optimal chunk size based on target time and worker performance
        let optimal_size = (base_performance * self.config.target_chunk_time) as u64;
        
        // Apply adaptive scaling based on job progress
        let progress_factor = self.calculate_progress_scaling_factor(remaining_points);
        let scaled_size = (optimal_size as f64 * progress_factor) as u64;

        // Clamp to configured bounds
        scaled_size
            .max(self.config.min_chunk_size)
            .min(self.config.max_chunk_size)
            .min(remaining_points)
    }

    fn calculate_progress_scaling_factor(&self, remaining_points: u64) -> f64 {
        let progress = 1.0 - (remaining_points as f64 / self.total_points as f64);
        
        // As job progresses, we can use larger chunks for efficiency
        // Early in the job: smaller chunks for better load balancing
        // Late in the job: larger chunks for efficiency
        if progress < 0.2 {
            // Early phase - use smaller chunks
            0.7
        } else if progress < 0.8 {
            // Middle phase - normal chunks
            1.0
        } else {
            // Late phase - larger chunks for efficiency
            1.5
        }
    }

    pub fn update_worker_performance(&mut self, performance: ChunkPerformance) {
        // Add to history
        self.chunk_history.push(performance.clone());
        
        // Keep history size manageable
        if self.chunk_history.len() > self.config.performance_history_size {
            self.chunk_history.remove(0);
        }

        // Update worker profile
        let (recent_performance, profile_ptr): (Vec<f64>, *mut WorkerPerformanceProfile) = {
            let profile = self.worker_performance
                .entry(performance.worker_id.clone())
                .or_insert_with(|| WorkerPerformanceProfile {
                    worker_id: performance.worker_id.clone(),
                    avg_points_per_second: 0.0,
                    recent_performance: Vec::new(),
                    last_updated: current_timestamp(),
                    reliability_score: 1.0,
                });

            // Update performance metrics
            profile.recent_performance.push(performance.points_per_second);
            if profile.recent_performance.len() > 10 {
                profile.recent_performance.remove(0);
            }

            // Calculate weighted average (recent performance counts more)
            let mut weighted_sum = 0.0;
            let mut total_weight = 0.0;
            for (i, perf) in profile.recent_performance.iter().enumerate() {
                let weight = (i + 1) as f64; // More recent = higher weight
                weighted_sum += perf * weight;
                total_weight += weight;
            }
            
            profile.avg_points_per_second = if total_weight > 0.0 {
                weighted_sum / total_weight
            } else {
                performance.points_per_second
            };

            profile.last_updated = current_timestamp();

            (profile.recent_performance.clone(), profile as *mut _)
        };

        // Now update reliability score outside the mutable borrow
        let variance = self.calculate_performance_variance(&recent_performance);
        unsafe {
            if let Some(profile) = profile_ptr.as_mut() {
                profile.reliability_score = (1.0 - variance).max(0.1);
            }
        }
    }

    fn calculate_performance_variance(&self, performances: &[f64]) -> f64 {
        if performances.len() < 2 {
            return 0.0;
        }

        let mean = performances.iter().sum::<f64>() / performances.len() as f64;
        let variance = performances.iter()
            .map(|&p| (p - mean).powi(2))
            .sum::<f64>() / performances.len() as f64;
        
        // Normalize variance to 0-1 range
        (variance / (mean * mean)).min(1.0)
    }

    pub fn get_worker_ranking(&self) -> Vec<(String, f64)> {
        let mut rankings = Vec::new();
        
        for (worker_id, profile) in &self.worker_performance {
            let score = profile.avg_points_per_second * profile.reliability_score;
            rankings.push((worker_id.clone(), score));
        }
        
        rankings.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        rankings
    }

    pub fn generate_adaptive_chunks(&mut self, remaining_points: u64, _available_workers: &[String]) -> Vec<AdaptiveChunk> {
        let mut chunks = Vec::new();
        let mut current_point = 0u64;
        
        // Sort workers by performance
        let worker_rankings = self.get_worker_ranking();
        let ranked_workers: Vec<String> = worker_rankings.into_iter().map(|(id, _)| id).collect();
        
        while current_point < remaining_points {
            let chunk_size = if let Some(best_worker) = ranked_workers.first() {
                self.calculate_optimal_chunk_size(best_worker, remaining_points - current_point)
            } else {
                self.config.min_chunk_size
            };

            let end_point = (current_point + chunk_size).min(remaining_points);
            
            // Calculate difficulty based on position in the computation
            let difficulty = self.calculate_chunk_difficulty(current_point, end_point);
            
            chunks.push(AdaptiveChunk {
                chunk_id: self.current_chunk_id,
                start_point: current_point,
                end_point,
                estimated_difficulty: difficulty,
                assigned_worker: None,
                priority: self.calculate_chunk_priority(difficulty, self.current_chunk_id),
            });

            current_point = end_point;
            self.current_chunk_id += 1;
        }

        // Sort by priority (highest first)
        chunks.sort_by(|a, b| b.priority.partial_cmp(&a.priority).unwrap());
        chunks
    }

    fn calculate_chunk_difficulty(&self, start_point: u64, _end_point: u64) -> f64 {
        // For Pi computation, difficulty is relatively uniform
        // But we can add some variation based on position
        let position = start_point as f64 / self.total_points as f64;
        
        // Slight variation to help with load balancing
        0.8 + 0.4 * (position * 2.0 * std::f64::consts::PI).sin()
    }

    fn calculate_chunk_priority(&self, difficulty: f64, chunk_id: u64) -> f64 {
        // Priority based on difficulty and chunk ID
        // Higher difficulty chunks get higher priority
        // Earlier chunks get slightly higher priority for better progress visibility
        let difficulty_weight = 0.7;
        let position_weight = 0.3;
        
        let position_factor = 1.0 - (chunk_id as f64 / 1000.0).min(1.0);
        
        difficulty_weight * difficulty + position_weight * position_factor
    }

    pub fn select_best_worker_for_chunk(&self, chunk: &AdaptiveChunk, available_workers: &[String]) -> Option<String> {
        if available_workers.is_empty() {
            return None;
        }

        let mut best_worker = None;
        let mut best_score = f64::NEG_INFINITY;

        for worker_id in available_workers {
            let profile = self.worker_performance.get(worker_id);
            let performance = profile.map(|p| p.avg_points_per_second).unwrap_or(1000.0);
            let reliability = profile.map(|p| p.reliability_score).unwrap_or(0.5);
            
            // Score based on performance, reliability, and chunk difficulty
            let score = if chunk.estimated_difficulty > 0.8 {
                // High difficulty chunks benefit from high-performance, reliable workers
                performance * reliability * 2.0
            } else {
                // Low difficulty chunks can be handled by any worker
                performance * reliability
            };

            if score > best_score {
                best_score = score;
                best_worker = Some(worker_id.clone());
            }
        }

        best_worker
    }

    pub fn get_performance_metrics(&self) -> HashMap<String, f64> {
        let mut metrics = HashMap::new();
        
        if self.chunk_history.is_empty() {
            return metrics;
        }

        let total_chunks = self.chunk_history.len();
        let total_points = self.chunk_history.iter().map(|c| c.points_processed).sum::<u64>();
        let total_time = self.chunk_history.iter().map(|c| c.computation_time).sum::<f64>();
        
        metrics.insert("total_chunks_processed".to_string(), total_chunks as f64);
        metrics.insert("total_points_processed".to_string(), total_points as f64);
        metrics.insert("total_computation_time".to_string(), total_time);
        metrics.insert("overall_points_per_second".to_string(), 
            if total_time > 0.0 { total_points as f64 / total_time } else { 0.0 });
        
        // Worker-specific metrics
        let mut worker_stats = HashMap::new();
        for performance in &self.chunk_history {
            let entry = worker_stats.entry(performance.worker_id.clone()).or_insert((0u64, 0.0));
            entry.0 += performance.points_processed;
            entry.1 += performance.computation_time;
        }
        
        for (worker_id, (points, time)) in worker_stats {
            let pps = if time > 0.0 { points as f64 / time } else { 0.0 };
            metrics.insert(format!("worker_{}_points_per_second", worker_id), pps);
        }
        
        metrics
    }

    pub fn adjust_configuration(&mut self) {
        if !self.config.enabled {
            return;
        }

        let metrics = self.get_performance_metrics();
        let avg_pps = metrics.get("overall_points_per_second").copied().unwrap_or(1000.0);
        
        // Adjust target chunk time based on overall performance
        if avg_pps > 2000.0 {
            // High performance - can use longer target times
            self.config.target_chunk_time = (self.config.target_chunk_time * 1.1).min(10.0);
        } else if avg_pps < 500.0 {
            // Low performance - use shorter target times
            self.config.target_chunk_time = (self.config.target_chunk_time * 0.9).max(1.0);
        }
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
} 