// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! The behavioural surface of an element.
//!
//! `Element` is declarative; `ElementRuntime` is behavioural. The
//! executor pairs each element with a concrete `ElementRuntime`
//! implementation (via `ElementRuntimeRegistry`) at execution time and
//! calls into this trait to materialize, observe, and tear down
//! resources.
//!
//! The trait uses the `async_trait` attribute pending stable
//! async-fn-in-dyn-trait (SRD-0007 D19). All implementations live in
//! the hyperplane tier; paramodel defines the trait shape.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use jiff::Timestamp;
use crate::{ParameterName, TrialId, Value};
use crate::trial::Trial;
use serde::{Deserialize, Serialize};

use crate::element::Element;
use crate::error::Result;
use crate::lifecycle::{LiveStatusSummary, StateTransition};

// ---------------------------------------------------------------------------
// TrialContext.
// ---------------------------------------------------------------------------

/// Read-only trial context passed to [`ElementRuntime`] lifecycle hooks.
///
/// Carries a reference to the owning [`Trial`] (defined in
/// `paramodel-trials`) so runtime implementations can read bound
/// parameter values and trial identity.
#[derive(Debug, Clone)]
pub struct TrialContext {
    /// The trial's id.
    pub trial_id:  TrialId,
    /// Shared reference to the trial. Cloning the `Arc` is cheap.
    pub trial:     Arc<Trial>,
    /// Observation timestamp.
    pub timestamp: Timestamp,
}

// ---------------------------------------------------------------------------
// ResolvedConfiguration / MaterializationOutputs.
// ---------------------------------------------------------------------------

/// Fully-interpolated configuration values handed to
/// [`ElementRuntime::materialize`]. Every entry is a concrete
/// [`Value`]; all tokens have been resolved.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResolvedConfiguration(BTreeMap<ParameterName, Value>);

impl ResolvedConfiguration {
    /// Empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a resolved value. Returns the previous value
    /// for `name`, if any.
    pub fn insert(&mut self, name: ParameterName, value: Value) -> Option<Value> {
        self.0.insert(name, value)
    }

    /// Look up a resolved value.
    #[must_use]
    pub fn get(&self, name: &ParameterName) -> Option<&Value> {
        self.0.get(name)
    }

    /// Sorted-by-key iterator.
    pub fn iter(&self) -> impl Iterator<Item = (&ParameterName, &Value)> {
        self.0.iter()
    }

    /// Map size.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(ParameterName, Value)> for ResolvedConfiguration {
    fn from_iter<I: IntoIterator<Item = (ParameterName, Value)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// Typed values an element publishes after materialization. Keyed by
/// `result_parameters` names; consumed by downstream elements that
/// reference the export via tokens.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MaterializationOutputs(BTreeMap<ParameterName, Value>);

impl MaterializationOutputs {
    /// Empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace an output value.
    pub fn insert(&mut self, name: ParameterName, value: Value) -> Option<Value> {
        self.0.insert(name, value)
    }

    /// Look up an output.
    #[must_use]
    pub fn get(&self, name: &ParameterName) -> Option<&Value> {
        self.0.get(name)
    }

    /// Sorted-by-key iterator.
    pub fn iter(&self) -> impl Iterator<Item = (&ParameterName, &Value)> {
        self.0.iter()
    }

    /// Map size.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<(ParameterName, Value)> for MaterializationOutputs {
    fn from_iter<I: IntoIterator<Item = (ParameterName, Value)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

// ---------------------------------------------------------------------------
// State observation.
// ---------------------------------------------------------------------------

/// Listener callback for [`ElementRuntime::observe_state`].
///
/// Implementations must deliver a synthetic initial transition from
/// `Unknown` to the current state as soon as the listener is
/// registered, so registration acts as catch-up for the observer.
pub type StateTransitionListener = Box<dyn Fn(StateTransition) + Send + Sync + 'static>;

/// Handle returned by [`ElementRuntime::observe_state`]. Calling
/// [`Self::cancel`] removes the listener.
pub trait StateObservation: Send + Sync + 'static {
    /// Cancel the observation.
    fn cancel(&self);
}

// ---------------------------------------------------------------------------
// ElementRuntime trait.
// ---------------------------------------------------------------------------

/// The async behavioural surface of an element.
///
/// Implementations live in the hyperplane tier (one per element type).
/// Paramodel ships a mock implementation in `paramodel-mock` for TCK
/// tests.
#[async_trait]
pub trait ElementRuntime: Send + Sync + 'static {
    /// Provision the element's concrete resources.
    ///
    /// `resolved` carries fully interpolated configuration. Returns
    /// typed values keyed by `result_parameters` names.
    async fn materialize(
        &self,
        resolved: &ResolvedConfiguration,
    ) -> Result<MaterializationOutputs>;

    /// Release the element's provisioned resources. Idempotent.
    async fn dematerialize(&self) -> Result<()>;

    /// Report the element's current operational state.
    async fn status_check(&self) -> LiveStatusSummary;

    /// Trial is starting — hook for per-trial setup. Default no-op.
    async fn on_trial_starting(&self, _ctx: &TrialContext) -> Result<()> {
        Ok(())
    }

    /// Trial is ending — hook for per-trial teardown. Default no-op.
    async fn on_trial_ending(&self, _ctx: &TrialContext) -> Result<()> {
        Ok(())
    }

    /// Register a state-transition listener.
    ///
    /// Implementations deliver a synthetic `Unknown → current`
    /// transition immediately so the observer doesn't miss the
    /// current state.
    fn observe_state(
        &self,
        listener: StateTransitionListener,
    ) -> Box<dyn StateObservation>;
}

// ---------------------------------------------------------------------------
// ElementRuntimeRegistry.
// ---------------------------------------------------------------------------

/// Host-provided service pairing each [`Element`] declaration with a
/// concrete [`ElementRuntime`]. Dispatch is typically on the element's
/// `type` label.
pub trait ElementRuntimeRegistry: Send + Sync + std::fmt::Debug + 'static {
    /// Pick (or construct) a runtime for this element.
    fn runtime_for(&self, element: &Element) -> Result<Arc<dyn ElementRuntime>>;
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    use ulid::Ulid;

    use super::*;
    use crate::lifecycle::OperationalState;

    fn tid() -> TrialId {
        TrialId::from_ulid(Ulid::from_parts(1_700_000_000_000, 1))
    }

    fn trial() -> Trial {
        Trial::builder()
            .id(tid())
            .assignments(crate::Assignments::empty())
            .build()
    }

    // A minimal in-memory runtime for verifying the trait shape.
    #[derive(Debug)]
    struct MockRuntime {
        materialized: AtomicBool,
        trial_starts: AtomicUsize,
    }

    #[async_trait]
    impl ElementRuntime for MockRuntime {
        async fn materialize(
            &self,
            _resolved: &ResolvedConfiguration,
        ) -> Result<MaterializationOutputs> {
            self.materialized.store(true, Ordering::SeqCst);
            Ok(MaterializationOutputs::new())
        }

        async fn dematerialize(&self) -> Result<()> {
            self.materialized.store(false, Ordering::SeqCst);
            Ok(())
        }

        async fn status_check(&self) -> LiveStatusSummary {
            LiveStatusSummary {
                state:   if self.materialized.load(Ordering::SeqCst) {
                    OperationalState::Ready
                } else {
                    OperationalState::Inactive
                },
                summary: "mock".to_owned(),
            }
        }

        async fn on_trial_starting(&self, _ctx: &TrialContext) -> Result<()> {
            self.trial_starts.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn observe_state(
            &self,
            _listener: StateTransitionListener,
        ) -> Box<dyn StateObservation> {
            Box::new(NoopObservation)
        }
    }

    #[derive(Debug)]
    struct NoopObservation;
    impl StateObservation for NoopObservation {
        fn cancel(&self) {}
    }

    #[tokio::test]
    async fn mock_runtime_materialize_and_status_check() {
        let rt = MockRuntime {
            materialized: AtomicBool::new(false),
            trial_starts: AtomicUsize::new(0),
        };
        let r = rt.status_check().await;
        assert_eq!(r.state, OperationalState::Inactive);
        rt.materialize(&ResolvedConfiguration::new()).await.unwrap();
        let r = rt.status_check().await;
        assert_eq!(r.state, OperationalState::Ready);
        rt.dematerialize().await.unwrap();
    }

    #[tokio::test]
    async fn mock_runtime_trial_hooks_dispatch() {
        let rt = MockRuntime {
            materialized: AtomicBool::new(false),
            trial_starts: AtomicUsize::new(0),
        };
        let ctx = TrialContext {
            trial_id:  tid(),
            trial:     Arc::new(trial()),
            timestamp: Timestamp::from_second(0).unwrap(),
        };
        rt.on_trial_starting(&ctx).await.unwrap();
        rt.on_trial_ending(&ctx).await.unwrap();
        assert_eq!(rt.trial_starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn resolved_configuration_iter_is_sorted() {
        let mut rc = ResolvedConfiguration::new();
        rc.insert(
            ParameterName::new("zebra").unwrap(),
            Value::integer(ParameterName::new("zebra").unwrap(), 1, None),
        );
        rc.insert(
            ParameterName::new("apple").unwrap(),
            Value::integer(ParameterName::new("apple").unwrap(), 2, None),
        );
        let names: Vec<&str> = rc.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["apple", "zebra"]);
    }

    #[test]
    fn materialization_outputs_serde_roundtrip() {
        let mut o = MaterializationOutputs::new();
        o.insert(
            ParameterName::new("endpoint").unwrap(),
            Value::string(
                ParameterName::new("endpoint").unwrap(),
                "http://example:4567",
                None,
            ),
        );
        let json = serde_json::to_string(&o).unwrap();
        let back: MaterializationOutputs = serde_json::from_str(&json).unwrap();
        assert_eq!(o, back);
    }
}
