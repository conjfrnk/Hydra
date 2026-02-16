//! Hydra - Grid-Aware Scheduling Module
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
pub struct GridRegion {
    pub region_id: String,
    pub carbon_intensity: f64, // gCO2/kWh
    pub electricity_cost: f64,  // $/kWh
    pub renewable_percentage: f64, // 0.0 to 1.0
    pub timezone: String,
    pub last_updated: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerGridInfo {
    pub worker_id: String,
    pub region_id: Option<String>,
    pub estimated_power_consumption: f64, // Watts
    pub carbon_intensity: f64, // gCO2/kWh
    pub electricity_cost: f64,  // $/kWh
    pub last_updated: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GridAwareConfig {
    pub enabled: bool,
    pub carbon_weight: f64,      // Weight for carbon optimization (0.0 to 1.0)
    pub cost_weight: f64,        // Weight for cost optimization (0.0 to 1.0)
    pub renewable_weight: f64,   // Weight for renewable energy preference (0.0 to 1.0)
    pub update_interval_seconds: u64,
}

pub struct GridAwareScheduler {
    regions: Arc<Mutex<HashMap<String, GridRegion>>>,
    worker_grid_info: Arc<Mutex<HashMap<String, WorkerGridInfo>>>,
    carbon_intensity_api: Option<CarbonIntensityAPI>,
}

impl GridAwareScheduler {
    pub fn new(config: GridAwareConfig) -> Self {
        let carbon_intensity_api = if config.enabled {
            Some(CarbonIntensityAPI::new())
        } else {
            None
        };

        Self {
            regions: Arc::new(Mutex::new(HashMap::new())),
            worker_grid_info: Arc::new(Mutex::new(HashMap::new())),
            carbon_intensity_api,
        }
    }

    pub async fn update_worker_grid_info(
        &self,
        worker_id: &str,
        region_id: Option<String>,
        estimated_power: f64,
    ) -> Result<()> {
        let mut worker_info = self.worker_grid_info.lock().await;
        
        let carbon_intensity = if let Some(region_id) = &region_id {
            self.get_region_carbon_intensity(region_id).await?
        } else {
            500.0 // Default carbon intensity if region unknown
        };

        let electricity_cost = if let Some(region_id) = &region_id {
            self.get_region_electricity_cost(region_id).await?
        } else {
            0.12 // Default cost if region unknown
        };

        worker_info.insert(worker_id.to_string(), WorkerGridInfo {
            worker_id: worker_id.to_string(),
            region_id,
            estimated_power_consumption: estimated_power,
            carbon_intensity,
            electricity_cost,
            last_updated: current_timestamp(),
        });

        Ok(())
    }



    async fn get_region_carbon_intensity(&self, region_id: &str) -> Result<f64> {
        if let Some(api) = &self.carbon_intensity_api {
            api.get_carbon_intensity(region_id).await
        } else {
            // Fallback to default values based on region
            Ok(match region_id {
                "us-west" => 300.0,  // California - lots of renewables
                "us-east" => 450.0,  // More coal-heavy
                "eu-west" => 250.0,  // Europe - good renewables
                "asia-pacific" => 600.0, // Coal-heavy regions
                _ => 500.0, // Default
            })
        }
    }

    async fn get_region_electricity_cost(&self, region_id: &str) -> Result<f64> {
        // Fallback to default values based on region
        Ok(match region_id {
            "us-west" => 0.15,  // California
            "us-east" => 0.12,  // Eastern US
            "eu-west" => 0.20,  // Europe
            "asia-pacific" => 0.08, // Asia
            _ => 0.12, // Default
        })
    }



    pub async fn get_sustainability_metrics(&self) -> HashMap<String, f64> {
        let worker_info = self.worker_grid_info.lock().await;
        let mut metrics = HashMap::new();

        let mut total_carbon = 0.0;
        let mut total_cost = 0.0;
        let mut total_renewable = 0.0;
        let mut worker_count = 0;

        for info in worker_info.values() {
            total_carbon += info.carbon_intensity;
            total_cost += info.electricity_cost;
            
            if let Some(region_id) = &info.region_id {
                if let Some(region) = self.regions.lock().await.get(region_id) {
                    total_renewable += region.renewable_percentage;
                }
            }
            
            worker_count += 1;
        }

        if worker_count > 0 {
            metrics.insert("avg_carbon_intensity".to_string(), total_carbon / worker_count as f64);
            metrics.insert("avg_electricity_cost".to_string(), total_cost / worker_count as f64);
            metrics.insert("avg_renewable_percentage".to_string(), total_renewable / worker_count as f64);
        }

        metrics
    }
}

struct CarbonIntensityAPI {
}

impl CarbonIntensityAPI {
    fn new() -> Self {
        Self {}
    }

    async fn get_carbon_intensity(&self, region_id: &str) -> Result<f64> {
        // In a real implementation, this would call the actual carbon intensity API
        // For now, we'll return mock data based on region
        Ok(match region_id {
            "us-west" => 300.0,
            "us-east" => 450.0,
            "eu-west" => 250.0,
            "asia-pacific" => 600.0,
            _ => 500.0,
        })
    }


}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
} 