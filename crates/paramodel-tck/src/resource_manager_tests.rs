// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Conformance checks for [`paramodel_executor::ResourceManager`].

use paramodel_executor::{ResourceManager, ResourceRequest};

use crate::providers::ResourceManagerProvider;

fn req(cpu: f64, mem: u64) -> ResourceRequest {
    ResourceRequest {
        cpu_cores: cpu,
        memory_mb: mem,
        ..ResourceRequest::default()
    }
}

/// A default request should succeed — if the implementation caps at
/// zero by default that's a bug.
pub async fn tck_resource_manager_allocates_simple_request<P>(provider: &P)
where
    P: ResourceManagerProvider,
{
    let mgr = provider.fresh();
    let alloc = mgr.allocate(&req(1.0, 128)).await.expect("allocate ok");
    assert!(
        (alloc.granted.cpu_cores - 1.0).abs() < f64::EPSILON,
        "cpu mismatch: {}",
        alloc.granted.cpu_cores,
    );
    assert_eq!(alloc.granted.memory_mb, 128);
}

/// `can_allocate` agrees with an actual `allocate` for the same
/// request.
pub async fn tck_resource_manager_can_allocate_consistent<P>(provider: &P)
where
    P: ResourceManagerProvider,
{
    let mgr = provider.fresh();
    let r = req(1.0, 64);
    let says_yes = mgr.can_allocate(&r);
    if says_yes {
        let alloc = mgr.allocate(&r).await.expect("should succeed");
        mgr.release(alloc).await;
    }
    // No direct way to verify negative side without pool caps — the
    // positive direction is the key invariant.
}

/// `release` should decrement `current_usage().active_allocations`.
pub async fn tck_resource_manager_release_decrements_usage<P>(provider: &P)
where
    P: ResourceManagerProvider,
{
    let mgr = provider.fresh();
    let before = mgr.current_usage().active_allocations;
    let alloc = mgr.allocate(&req(0.5, 32)).await.expect("allocate");
    let during = mgr.current_usage().active_allocations;
    mgr.release(alloc).await;
    let after = mgr.current_usage().active_allocations;
    assert_eq!(during, before + 1);
    assert_eq!(after, before);
}

/// `available()` produces a snapshot that's consistent with itself
/// — two calls in a row return the same numbers (no allocations in
/// between).
pub fn tck_resource_manager_available_snapshot_is_stable<P>(provider: &P)
where
    P: ResourceManagerProvider,
{
    let mgr = provider.fresh();
    let a = mgr.available();
    let b = mgr.available();
    assert_eq!(a.cpu_cores_free.to_bits(), b.cpu_cores_free.to_bits());
    assert_eq!(a.memory_mb_free, b.memory_mb_free);
}

/// `pools()` is always safe to call on a fresh manager — may be
/// empty, must not panic.
pub fn tck_resource_manager_pools_safe<P: ResourceManagerProvider>(provider: &P) {
    let mgr = provider.fresh();
    let _ = mgr.pools();
}
