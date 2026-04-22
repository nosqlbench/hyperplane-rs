// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `ResourceManager` trait + supporting types.
//!
//! V0.1 ships a `DefaultResourceManager` that always admits; quotas
//! and sub-pools are defined in the trait surface for future backends.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::ResourceError;
use crate::ids::AllocationId;

/// Ask for capacity.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceRequest {
    /// Fractional CPU-core ask.
    pub cpu_cores: f64,
    /// RSS memory ask in megabytes.
    pub memory_mb: u64,
    /// Storage ask in gigabytes.
    pub storage_gb: u64,
    /// Network throughput in Gbps.
    pub network_gbps: f64,
    /// Adopter-defined resources (`"gpu"`, `"license_seats"`, …).
    #[serde(default)]
    pub custom: BTreeMap<String, u64>,
    /// Optional pool name.
    pub pool: Option<String>,
    /// Owning user / tenant.
    pub owner: Option<String>,
    /// Expected hold duration.
    pub duration: Option<Duration>,
}

/// Granted allocation handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceAllocation {
    /// Allocation id.
    pub id: AllocationId,
    /// What the manager actually granted.
    pub granted: ResourceCapacity,
    /// Pool the allocation came from, if any.
    pub pool: Option<String>,
    /// When the allocation was granted.
    pub allocated_at: Timestamp,
}

/// Quantised capacity — a resource shape shared by requests, grants,
/// and pool capacities.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceCapacity {
    /// CPU cores.
    pub cpu_cores: f64,
    /// Memory in megabytes.
    pub memory_mb: u64,
    /// Storage in gigabytes.
    pub storage_gb: u64,
    /// Network throughput in Gbps.
    pub network_gbps: f64,
    /// Adopter-defined custom resources.
    #[serde(default)]
    pub custom: BTreeMap<String, u64>,
}

/// Current-availability snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceSnapshot {
    /// Free CPU cores.
    pub cpu_cores_free: f64,
    /// Free memory in megabytes.
    pub memory_mb_free: u64,
    /// Free storage in gigabytes.
    pub storage_gb_free: u64,
    /// Free network throughput in Gbps.
    pub network_gbps_free: f64,
    /// Free custom-resource counts.
    #[serde(default)]
    pub custom_free: BTreeMap<String, u64>,
}

/// Aggregate usage across the manager.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResourceUsage {
    /// Allocations currently held.
    pub active_allocations: u32,
    /// Total capacity granted to active allocations.
    pub total_granted: ResourceCapacity,
}

/// A named resource pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourcePool {
    /// Pool name.
    pub name: String,
    /// Capacity cap.
    pub capacity: ResourceCapacity,
    /// Pool priority when contending with other pools.
    pub priority: PoolPriority,
}

/// Contention priority between pools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolPriority {
    /// Background work.
    Low,
    /// Normal priority.
    Normal,
    /// Elevated priority.
    High,
    /// System-critical.
    Critical,
}

/// Per-user cap the manager enforces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceQuota {
    /// Owning user.
    pub user:     String,
    /// Maximum capacity.
    pub capacity: ResourceCapacity,
}

/// Current usage for one user against their quota.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaUsage {
    /// Owning user.
    pub user:       String,
    /// Used.
    pub used:       ResourceCapacity,
    /// Remaining.
    pub available:  ResourceCapacity,
}

// ---------------------------------------------------------------------------
// ResourceManager trait.
// ---------------------------------------------------------------------------

/// Host resource bookkeeper.
#[async_trait]
pub trait ResourceManager: Send + Sync + 'static {
    /// Request an allocation.
    async fn allocate(
        &self,
        request: &ResourceRequest,
    ) -> Result<ResourceAllocation, ResourceError>;

    /// Release an allocation.
    async fn release(&self, allocation: ResourceAllocation);

    /// Check whether a request could be satisfied right now.
    fn can_allocate(&self, request: &ResourceRequest) -> bool;

    /// Current availability.
    fn available(&self) -> ResourceSnapshot;

    /// Aggregate usage across the manager.
    fn current_usage(&self) -> ResourceUsage;

    /// Register a new pool. V0.1 default impls may ignore.
    fn create_pool(&self, name: &str, capacity: &ResourceCapacity, priority: PoolPriority);

    /// All registered pools.
    fn pools(&self) -> Vec<ResourcePool>;

    /// Set / update a user quota.
    fn set_quota(&self, quota: ResourceQuota);

    /// Query quota usage for a user.
    fn quota_usage(&self, user: &str) -> QuotaUsage;
}

// ---------------------------------------------------------------------------
// DefaultResourceManager — unbounded reference impl.
// ---------------------------------------------------------------------------

/// Unbounded, in-memory resource manager. Always admits; no pools,
/// no quotas; records active allocations for usage reporting.
#[derive(Debug, Default)]
pub struct DefaultResourceManager {
    active: Mutex<Vec<ResourceAllocation>>,
}

impl DefaultResourceManager {
    /// Construct.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            active: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ResourceManager for DefaultResourceManager {
    async fn allocate(
        &self,
        request: &ResourceRequest,
    ) -> Result<ResourceAllocation, ResourceError> {
        let allocation = ResourceAllocation {
            id:      AllocationId::from_ulid(Ulid::from_parts(
                u64::try_from(jiff::Timestamp::now().as_second().max(0)).unwrap_or(0),
                0,
            )),
            granted: ResourceCapacity {
                cpu_cores:    request.cpu_cores,
                memory_mb:    request.memory_mb,
                storage_gb:   request.storage_gb,
                network_gbps: request.network_gbps,
                custom:       request.custom.clone(),
            },
            pool:         request.pool.clone(),
            allocated_at: jiff::Timestamp::now(),
        };
        self.active.lock().expect("poisoned").push(allocation.clone());
        Ok(allocation)
    }

    async fn release(&self, allocation: ResourceAllocation) {
        let mut active = self.active.lock().expect("poisoned");
        active.retain(|a| a.id != allocation.id);
    }

    fn can_allocate(&self, _request: &ResourceRequest) -> bool {
        true
    }

    fn available(&self) -> ResourceSnapshot {
        ResourceSnapshot {
            cpu_cores_free:    f64::MAX,
            memory_mb_free:    u64::MAX,
            storage_gb_free:   u64::MAX,
            network_gbps_free: f64::MAX,
            custom_free:       BTreeMap::new(),
        }
    }

    fn current_usage(&self) -> ResourceUsage {
        let active = self.active.lock().expect("poisoned");
        let mut total = ResourceCapacity::default();
        for a in active.iter() {
            total.cpu_cores += a.granted.cpu_cores;
            total.memory_mb += a.granted.memory_mb;
            total.storage_gb += a.granted.storage_gb;
            total.network_gbps += a.granted.network_gbps;
            for (k, v) in &a.granted.custom {
                *total.custom.entry(k.clone()).or_insert(0) += *v;
            }
        }
        ResourceUsage {
            active_allocations: u32::try_from(active.len()).unwrap_or(u32::MAX),
            total_granted:      total,
        }
    }

    fn create_pool(&self, _name: &str, _capacity: &ResourceCapacity, _priority: PoolPriority) {
        // No-op in the unbounded manager.
    }

    fn pools(&self) -> Vec<ResourcePool> {
        Vec::new()
    }

    fn set_quota(&self, _quota: ResourceQuota) {
        // No-op.
    }

    fn quota_usage(&self, user: &str) -> QuotaUsage {
        QuotaUsage {
            user:      user.to_owned(),
            used:      ResourceCapacity::default(),
            available: ResourceCapacity::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn default_manager_always_admits_and_tracks() {
        let m = DefaultResourceManager::new();
        let req = ResourceRequest {
            cpu_cores: 2.0,
            ..Default::default()
        };
        assert!(m.can_allocate(&req));
        let a = m.allocate(&req).await.unwrap();
        assert_eq!(m.current_usage().active_allocations, 1);
        m.release(a).await;
        assert_eq!(m.current_usage().active_allocations, 0);
    }
}
