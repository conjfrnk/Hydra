//! Hydra - Tokenized Incentives Module
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
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenReward {
    pub worker_id: String,
    pub tokens_earned: f64,
    pub computation_units: u64,
    pub carbon_offset: f64, // kg CO2 saved
    pub renewable_energy_used: f64, // kWh from renewables
    pub timestamp: u64,
    pub job_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerTokenBalance {
    pub worker_id: String,
    pub total_tokens: f64,
    pub total_computation: u64,
    pub total_carbon_offset: f64,
    pub total_renewable_energy: f64,
    pub last_updated: u64,
    pub reputation_score: f64, // 0.0 to 1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenIncentiveConfig {
    pub enabled: bool,
    pub base_token_rate: f64, // tokens per computation unit
    pub carbon_bonus_rate: f64, // bonus tokens per kg CO2 saved
    pub renewable_bonus_rate: f64, // bonus tokens per kWh renewable
    pub performance_multiplier: f64, // multiplier for high-performance workers
    pub reliability_bonus: f64, // bonus for reliable workers
    pub max_tokens_per_chunk: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenTransaction {
    pub transaction_id: String,
    pub from_worker: String,
    pub to_worker: Option<String>, // None for system rewards
    pub amount: f64,
    pub transaction_type: String, // "reward", "transfer", "penalty"
    pub timestamp: u64,
    pub description: String,
}

pub struct TokenIncentiveSystem {
    config: TokenIncentiveConfig,
    worker_balances: Arc<Mutex<HashMap<String, WorkerTokenBalance>>>,
    transaction_history: Arc<Mutex<Vec<TokenTransaction>>>,
    reward_history: Arc<Mutex<Vec<TokenReward>>>,
}

impl TokenIncentiveSystem {
    pub fn new(config: TokenIncentiveConfig) -> Self {
        Self {
            config,
            worker_balances: Arc::new(Mutex::new(HashMap::new())),
            transaction_history: Arc::new(Mutex::new(Vec::new())),
            reward_history: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn calculate_reward(
        &self,
        worker_id: &str,
        computation_units: u64,
        carbon_intensity: f64,
        renewable_percentage: f64,
        performance_score: f64,
        reliability_score: f64,
        job_id: &str,
    ) -> Result<TokenReward> {
        if !self.config.enabled {
            return Ok(TokenReward {
                worker_id: worker_id.to_string(),
                tokens_earned: 0.0,
                computation_units,
                carbon_offset: 0.0,
                renewable_energy_used: 0.0,
                timestamp: current_timestamp(),
                job_id: job_id.to_string(),
            });
        }

        // Base reward for computation
        let base_reward = computation_units as f64 * self.config.base_token_rate;

        // Calculate carbon offset (assuming 500W average power consumption)
        let power_consumption_watts = 500.0;
        let computation_time_hours = computation_units as f64 / 1000000.0; // Rough estimate
        let energy_consumed_kwh = (power_consumption_watts * computation_time_hours) / 1000.0;
        
        let grid_carbon_intensity = 500.0; // gCO2/kWh average
        let carbon_offset = energy_consumed_kwh * (grid_carbon_intensity - carbon_intensity) / 1000.0; // kg CO2
        
        // Calculate renewable energy used
        let renewable_energy_used = energy_consumed_kwh * renewable_percentage;

        // Calculate bonuses
        let carbon_bonus = carbon_offset * self.config.carbon_bonus_rate;
        let renewable_bonus = renewable_energy_used * self.config.renewable_bonus_rate;
        let performance_bonus = base_reward * (performance_score - 1.0) * self.config.performance_multiplier;
        let reliability_bonus = base_reward * reliability_score * self.config.reliability_bonus;

        let total_tokens = base_reward + carbon_bonus + renewable_bonus + performance_bonus + reliability_bonus;
        let capped_tokens = total_tokens.min(self.config.max_tokens_per_chunk);

        Ok(TokenReward {
            worker_id: worker_id.to_string(),
            tokens_earned: capped_tokens,
            computation_units,
            carbon_offset,
            renewable_energy_used,
            timestamp: current_timestamp(),
            job_id: job_id.to_string(),
        })
    }

    pub async fn distribute_reward(&self, reward: TokenReward) -> Result<()> {
        let mut balances = self.worker_balances.lock().await;
        let mut transactions = self.transaction_history.lock().await;
        let mut rewards = self.reward_history.lock().await;

        // Update worker balance
        let balance = balances.entry(reward.worker_id.clone()).or_insert_with(|| WorkerTokenBalance {
            worker_id: reward.worker_id.clone(),
            total_tokens: 0.0,
            total_computation: 0,
            total_carbon_offset: 0.0,
            total_renewable_energy: 0.0,
            last_updated: current_timestamp(),
            reputation_score: 1.0,
        });

        balance.total_tokens += reward.tokens_earned;
        balance.total_computation += reward.computation_units;
        balance.total_carbon_offset += reward.carbon_offset;
        balance.total_renewable_energy += reward.renewable_energy_used;
        balance.last_updated = current_timestamp();

        // Record transaction
        let transaction = TokenTransaction {
            transaction_id: format!("tx_{}", current_timestamp()),
            from_worker: "system".to_string(),
            to_worker: Some(reward.worker_id.clone()),
            amount: reward.tokens_earned,
            transaction_type: "reward".to_string(),
            timestamp: current_timestamp(),
            description: format!("Computation reward for job {}", reward.job_id),
        };
        transactions.push(transaction);

        // Record reward
        rewards.push(reward);

        Ok(())
    }

    pub async fn get_worker_balance(&self, worker_id: &str) -> Option<WorkerTokenBalance> {
        let balances = self.worker_balances.lock().await;
        balances.get(worker_id).cloned()
    }

    pub async fn get_leaderboard(&self) -> Vec<WorkerTokenBalance> {
        let balances = self.worker_balances.lock().await;
        let mut leaderboard: Vec<WorkerTokenBalance> = balances.values().cloned().collect();
        leaderboard.sort_by(|a, b| b.total_tokens.partial_cmp(&a.total_tokens).unwrap());
        leaderboard
    }

    pub async fn transfer_tokens(
        &self,
        from_worker: &str,
        to_worker: &str,
        amount: f64,
    ) -> Result<bool> {
        let mut balances = self.worker_balances.lock().await;
        let mut transactions = self.transaction_history.lock().await;

        // Check if both workers exist
        if !balances.contains_key(from_worker) || !balances.contains_key(to_worker) {
            return Ok(false); // Worker not found
        }

        {
            let from_balance = balances.get_mut(from_worker).unwrap();
            if from_balance.total_tokens < amount {
                return Ok(false); // Insufficient funds
            }
            from_balance.total_tokens -= amount;
        }
        
        {
            let to_balance = balances.get_mut(to_worker).unwrap();
            to_balance.total_tokens += amount;
        }

        let transaction = TokenTransaction {
            transaction_id: format!("tx_{}", current_timestamp()),
            from_worker: from_worker.to_string(),
            to_worker: Some(to_worker.to_string()),
            amount,
            transaction_type: "transfer".to_string(),
            timestamp: current_timestamp(),
            description: "Worker-to-worker transfer".to_string(),
        };
        transactions.push(transaction);

        Ok(true)
    }

    pub async fn get_sustainability_metrics(&self) -> HashMap<String, f64> {
        let balances = self.worker_balances.lock().await;
        let mut metrics = HashMap::new();

        let mut total_tokens = 0.0;
        let mut total_computation = 0u64;
        let mut total_carbon_offset = 0.0;
        let mut total_renewable_energy = 0.0;
        let mut worker_count = 0;

        for balance in balances.values() {
            total_tokens += balance.total_tokens;
            total_computation += balance.total_computation;
            total_carbon_offset += balance.total_carbon_offset;
            total_renewable_energy += balance.total_renewable_energy;
            worker_count += 1;
        }

        metrics.insert("total_tokens_distributed".to_string(), total_tokens);
        metrics.insert("total_computation_units".to_string(), total_computation as f64);
        metrics.insert("total_carbon_offset_kg".to_string(), total_carbon_offset);
        metrics.insert("total_renewable_energy_kwh".to_string(), total_renewable_energy);
        metrics.insert("active_workers".to_string(), worker_count as f64);

        if worker_count > 0 {
            metrics.insert("avg_tokens_per_worker".to_string(), total_tokens / worker_count as f64);
            metrics.insert("avg_carbon_offset_per_worker".to_string(), total_carbon_offset / worker_count as f64);
        }

        metrics
    }

    pub async fn update_worker_reputation(&self, worker_id: &str, reliability_score: f64) -> Result<()> {
        let mut balances = self.worker_balances.lock().await;
        
        if let Some(balance) = balances.get_mut(worker_id) {
            // Update reputation based on reliability
            balance.reputation_score = (balance.reputation_score * 0.9 + reliability_score * 0.1).max(0.0).min(1.0);
        }

        Ok(())
    }

    pub async fn get_recent_transactions(&self, limit: usize) -> Vec<TokenTransaction> {
        let transactions = self.transaction_history.lock().await;
        let mut recent: Vec<TokenTransaction> = transactions.iter().cloned().collect();
        recent.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        recent.truncate(limit);
        recent
    }

    pub async fn get_reward_history(&self, worker_id: &str, limit: usize) -> Vec<TokenReward> {
        let rewards = self.reward_history.lock().await;
        let mut worker_rewards: Vec<TokenReward> = rewards.iter()
            .filter(|r| r.worker_id == worker_id)
            .cloned()
            .collect();
        worker_rewards.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        worker_rewards.truncate(limit);
        worker_rewards
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
} 