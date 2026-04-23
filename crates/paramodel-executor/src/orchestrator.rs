// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Orchestrator` trait + `DefaultOrchestrator`.
//!
//! The orchestrator translates an `AtomicStep` into a call on a
//! concrete `ElementRuntime`. V0.1 takes a pre-resolved
//! `BTreeMap<ElementName, Arc<dyn ElementRuntime>>` rather than an
//! `ElementRuntimeRegistry`: `ExecutionPlan` doesn't re-carry the
//! authored `Element` records a registry would need to dispatch
//! against. Registry-based resolution arrives when the compiler
//! starts attaching element records to the plan's execution metadata.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use paramodel_elements::{
    ElementName, ElementRuntime, HealthCheckSpec, LiveStatusSummary, MaterializationOutputs,
    ParameterName, Trial, Value,
};
use paramodel_plan::{AtomicStep, ExecutionPlan, InstanceId, OutputSelector, ShutdownReason};
use paramodel_trials::{ArtifactRef, ErrorInfo};
use serde::{Deserialize, Serialize};

use crate::error::OrchestratorError;

/// Outcome of one `execute_step` call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StepOutcome {
    /// Step finished cleanly.
    Completed {
        /// Wall-clock elapsed.
        duration: Duration,
        /// Typed outputs from a `Deploy` step.
        #[serde(default)]
        materialization_outputs: MaterializationOutputs,
        /// Metrics captured by a `SaveOutput` step.
        #[serde(default)]
        metrics: BTreeMap<ParameterName, Value>,
        /// Artifacts captured by a `SaveOutput` / `Teardown`.
        #[serde(default)]
        artifacts: Vec<ArtifactRef>,
    },
    /// Step was skipped.
    Skipped {
        /// Reason.
        reason: String,
    },
    /// Step failed.
    Failed {
        /// Error payload.
        error: ErrorInfo,
    },
}

// ---------------------------------------------------------------------------
// Orchestrator trait.
// ---------------------------------------------------------------------------

/// System runtime that bridges the graph to `ElementRuntime`s.
#[async_trait]
pub trait Orchestrator: Send + Sync + 'static {
    /// Prepare for a run — register every element, resolve runtimes,
    /// acquire long-lived resources.
    async fn prepare(&self, plan: &ExecutionPlan) -> Result<(), OrchestratorError>;

    /// Execute one step against the correct `ElementRuntime`.
    async fn execute_step(
        &self,
        plan: &ExecutionPlan,
        step: &AtomicStep,
    ) -> Result<StepOutcome, OrchestratorError>;

    /// Await an instance's readiness per its health-check spec.
    async fn await_ready(
        &self,
        instance_id: &InstanceId,
        spec:        &HealthCheckSpec,
    ) -> Result<(), OrchestratorError>;

    /// Deliver `on_trial_starting` to every listed non-trial element.
    async fn notify_trial_start(
        &self,
        trial:    &Trial,
        elements: &[ElementName],
    ) -> Result<(), OrchestratorError>;

    /// Deliver `on_trial_ending` to every listed non-trial element.
    async fn notify_trial_end(
        &self,
        trial:    &Trial,
        elements: &[ElementName],
        reason:   ShutdownReason,
    ) -> Result<(), OrchestratorError>;

    /// Final teardown of any live instances + resource release.
    async fn shutdown(&self) -> Result<(), OrchestratorError>;

    /// Query operational state of a live instance.
    async fn instance_state(&self, instance_id: &InstanceId) -> Option<LiveStatusSummary>;
}

// ---------------------------------------------------------------------------
// DefaultOrchestrator.
// ---------------------------------------------------------------------------

/// Reference orchestrator backed by a pre-resolved runtime map.
pub struct DefaultOrchestrator {
    runtimes: Mutex<BTreeMap<ElementName, Arc<dyn ElementRuntime>>>,
    /// Materialization outputs captured at `Deploy` time, keyed by
    /// `(element, instance_number)`. Consulted by `SaveOutput`.
    outputs:  Mutex<BTreeMap<(ElementName, u32), MaterializationOutputs>>,
}

impl std::fmt::Debug for DefaultOrchestrator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.runtimes.lock().expect("poisoned");
        f.debug_struct("DefaultOrchestrator")
            .field("runtimes", &guard.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl DefaultOrchestrator {
    /// Construct over a pre-resolved runtime map.
    #[must_use]
    pub fn new(runtimes: BTreeMap<ElementName, Arc<dyn ElementRuntime>>) -> Self {
        Self {
            runtimes: Mutex::new(runtimes),
            outputs:  Mutex::new(BTreeMap::new()),
        }
    }

    /// Empty orchestrator (for plans with no element steps).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            runtimes: Mutex::new(BTreeMap::new()),
            outputs:  Mutex::new(BTreeMap::new()),
        }
    }

    fn runtime_for(
        &self,
        element: &ElementName,
    ) -> Result<Arc<dyn ElementRuntime>, OrchestratorError> {
        self.runtimes
            .lock()
            .expect("poisoned")
            .get(element)
            .cloned()
            .ok_or_else(|| OrchestratorError::NoRuntimeForElement {
                element: element.as_str().to_owned(),
            })
    }
}

#[async_trait]
impl Orchestrator for DefaultOrchestrator {
    async fn prepare(&self, _plan: &ExecutionPlan) -> Result<(), OrchestratorError> {
        Ok(())
    }

    async fn execute_step(
        &self,
        _plan: &ExecutionPlan,
        step:  &AtomicStep,
    ) -> Result<StepOutcome, OrchestratorError> {
        match step {
            AtomicStep::Deploy {
                element, configuration, instance_number, ..
            } => {
                let rt = self.runtime_for(element)?;
                match rt.materialize(configuration).await {
                    Ok(outputs) => {
                        // Stash outputs for later `SaveOutput` lookups.
                        self.outputs
                            .lock()
                            .expect("poisoned")
                            .insert((element.clone(), *instance_number), outputs.clone());
                        Ok(StepOutcome::Completed {
                            duration:                Duration::ZERO,
                            materialization_outputs: outputs,
                            metrics:                 BTreeMap::new(),
                            artifacts:               Vec::new(),
                        })
                    }
                    // A runtime-level failure is a step-level failure,
                    // not an infrastructure error. Surface it as
                    // `StepOutcome::Failed` so the executor can honour
                    // the plan's `on_failure` policy.
                    Err(err) => Ok(StepOutcome::Failed {
                        error: ErrorInfo::builder()
                            .kind("MaterializeFailed".to_owned())
                            .message(format!("{err}"))
                            .build(),
                    }),
                }
            }
            AtomicStep::Teardown { element, .. } => {
                let rt = self.runtime_for(element)?;
                match rt.dematerialize().await {
                    Ok(()) => Ok(StepOutcome::Completed {
                        duration:                Duration::ZERO,
                        materialization_outputs: MaterializationOutputs::new(),
                        metrics:                 BTreeMap::new(),
                        artifacts:               Vec::new(),
                    }),
                    Err(err) => Ok(StepOutcome::Failed {
                        error: ErrorInfo::builder()
                            .kind("DematerializeFailed".to_owned())
                            .message(format!("{err}"))
                            .build(),
                    }),
                }
            }
            AtomicStep::Checkpoint { .. } | AtomicStep::Barrier { .. } => Ok(
                StepOutcome::Completed {
                    duration:                Duration::ZERO,
                    materialization_outputs: MaterializationOutputs::new(),
                    metrics:                 BTreeMap::new(),
                    artifacts:               Vec::new(),
                },
            ),
            // Trial-lifecycle steps are no-op successes at the
            // orchestrator layer. The executor is responsible for
            // aggregating them into `ExecutionResults.trial_results`
            // — see `DefaultExecutor::execute`.
            AtomicStep::TrialStart { .. } | AtomicStep::TrialEnd { .. } => {
                Ok(StepOutcome::Completed {
                    duration:                Duration::ZERO,
                    materialization_outputs: MaterializationOutputs::new(),
                    metrics:                 BTreeMap::new(),
                    artifacts:               Vec::new(),
                })
            }
            // `Await` targets a live instance; if the runtime is in
            // `Failed` state the step fails (executor then honours
            // `on_failure`).
            AtomicStep::Await { element, .. } => {
                let rt = self.runtime_for(element)?;
                let status = rt.status_check().await;
                if matches!(status.state, paramodel_elements::OperationalState::Failed) {
                    Ok(StepOutcome::Failed {
                        error: ErrorInfo::builder()
                            .kind("ElementFailed".to_owned())
                            .message(format!(
                                "element '{}' is in Failed state: {}",
                                element.as_str(),
                                status.summary,
                            ))
                            .build(),
                    })
                } else {
                    Ok(StepOutcome::Completed {
                        duration:                Duration::ZERO,
                        materialization_outputs: MaterializationOutputs::new(),
                        metrics:                 BTreeMap::new(),
                        artifacts:               Vec::new(),
                    })
                }
            }
            // `SaveOutput` reads typed output from a previously
            // deployed instance. Failure modes: the element is in
            // `Failed` state, no `Deploy` has been recorded for the
            // `(element, instance_number)` pair, or the selected
            // `ResultParameter` isn't present in the captured outputs.
            AtomicStep::SaveOutput {
                element, instance_number, output, ..
            } => {
                let rt = self.runtime_for(element)?;
                let status = rt.status_check().await;
                if matches!(status.state, paramodel_elements::OperationalState::Failed) {
                    return Ok(StepOutcome::Failed {
                        error: ErrorInfo::builder()
                            .kind("ElementFailed".to_owned())
                            .message(format!(
                                "element '{}' is in Failed state: {}",
                                element.as_str(),
                                status.summary,
                            ))
                            .build(),
                    });
                }
                let stash = self.outputs.lock().expect("poisoned");
                let outs = match stash.get(&(element.clone(), *instance_number)) {
                    Some(o) => o,
                    None => {
                        return Ok(StepOutcome::Failed {
                            error: ErrorInfo::builder()
                                .kind("NoOutputsForInstance".to_owned())
                                .message(format!(
                                    "no materialization outputs recorded for \
                                     ({}, {}); was Deploy run?",
                                    element.as_str(),
                                    instance_number,
                                ))
                                .build(),
                        });
                    }
                };
                match output {
                    OutputSelector::ResultParameter { parameter } => {
                        let Some(value) = outs.get(parameter) else {
                            return Ok(StepOutcome::Failed {
                                error: ErrorInfo::builder()
                                    .kind("MissingResultParameter".to_owned())
                                    .message(format!(
                                        "element '{}' did not produce parameter '{}'",
                                        element.as_str(),
                                        parameter.as_str(),
                                    ))
                                    .build(),
                            });
                        };
                        let mut metrics = BTreeMap::new();
                        metrics.insert(parameter.clone(), value.clone());
                        Ok(StepOutcome::Completed {
                            duration:                Duration::ZERO,
                            materialization_outputs: MaterializationOutputs::new(),
                            metrics,
                            artifacts:               Vec::new(),
                        })
                    }
                    OutputSelector::Volume { mount } => {
                        // Record a placeholder ArtifactRef keyed by
                        // the mount path. Real adapters will produce
                        // a content-addressed uri; here we echo the
                        // mount name so the trial's artifacts list
                        // reflects the capture request.
                        let artifact = paramodel_trials::ArtifactRef::builder()
                            .element(element.clone())
                            .name(mount.clone())
                            .uri(format!("mem://{}/{}/{}", element.as_str(), instance_number, mount))
                            .content_type("application/octet-stream".to_owned())
                            .build();
                        Ok(StepOutcome::Completed {
                            duration:                Duration::ZERO,
                            materialization_outputs: MaterializationOutputs::new(),
                            metrics:                 BTreeMap::new(),
                            artifacts:               vec![artifact],
                        })
                    }
                }
            }
        }
    }

    async fn await_ready(
        &self,
        _instance_id: &InstanceId,
        _spec:        &HealthCheckSpec,
    ) -> Result<(), OrchestratorError> {
        Ok(())
    }

    async fn notify_trial_start(
        &self,
        _trial:    &Trial,
        _elements: &[ElementName],
    ) -> Result<(), OrchestratorError> {
        Ok(())
    }

    async fn notify_trial_end(
        &self,
        _trial:    &Trial,
        _elements: &[ElementName],
        _reason:   ShutdownReason,
    ) -> Result<(), OrchestratorError> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), OrchestratorError> {
        self.runtimes.lock().expect("poisoned").clear();
        Ok(())
    }

    async fn instance_state(&self, _instance_id: &InstanceId) -> Option<LiveStatusSummary> {
        None
    }
}
