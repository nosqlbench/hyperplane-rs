// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `Compiler` trait and `DefaultCompiler`.
//!
//! Scope: the v0.1 compiler runs a trimmed version of reducto's
//! pipeline that covers the trivial case end-to-end.
//!
//! - Stage 1 (§1): full — `MixedRadixEnumerator`, `BindingStateComputer`,
//!   trial-element identification.
//! - Stage 2 (§2): trivial — seeds are implicit.
//! - Stage 3 (§3): Rules 1 (lifecycle expansion), 2 (only `Shared`
//!   edges), 7 (Start/End sentinels). Rules 3 / 4 / 5 / 6 / 8 are
//!   not yet implemented.
//! - Stage 4 (§4): linearization, trivial `ElementInstanceGraph`
//!   construction.
//!
//! Anything outside that envelope — non-`Shared` relationships,
//! command-mode elements, token-expression configuration entries,
//! coalescing, health gates, concurrency annotations, transitive
//! reduction, custom orderings, `compile_incremental` — surfaces
//! as an `E002 UnsupportedPlanFeature` diagnostic.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use jiff::Timestamp;
use paramodel_elements::{
    ConfigEntry, Element, ElementName, ParameterName, RelationshipType, ResolvedConfiguration,
    ShutdownSemantics, TrialId, Value,
};
use paramodel_plan::{
    AtomicStep, BarrierId, BarrierKind, CheckpointId, ElementInstance, ElementInstanceGraph,
    ExecutionGraph, ExecutionPlan, ExecutionPlanId, ExecutionPlanMetadata, InstanceId,
    InstanceScope, PerformanceMetrics, ResourceRequirements, ShutdownReason, StepHeader,
    StepId, TestPlan, TimeoutAction,
};
use ulid::Ulid;

use crate::binding::BindingStateComputer;
use crate::enumerator::MixedRadixEnumerator;
use crate::error::{CompilationDiagnostic, CompilationError, DiagnosticLocation, WarningCode, error};
use crate::options::CompilerOptions;
use crate::trial_element::identify_trial_elements;

/// Turns a `TestPlan` into an `ExecutionPlan`.
pub trait Compiler: Send + Sync + 'static {
    /// Full compile.
    fn compile(&self, plan: &TestPlan) -> Result<ExecutionPlan, CompilationError>;

    /// Produce every diagnostic a full compile would, without actually
    /// building the execution plan. Useful for IDE-style early
    /// feedback.
    fn validate(&self, plan: &TestPlan) -> Vec<CompilationDiagnostic>;

    /// Compiler version string.
    fn version(&self) -> &str;

    /// This compiler's options.
    fn options(&self) -> &CompilerOptions;
}

// ---------------------------------------------------------------------------
// DefaultCompiler.
// ---------------------------------------------------------------------------

/// Reference compiler. See module docs for scope.
#[derive(Debug, Clone)]
pub struct DefaultCompiler {
    options: CompilerOptions,
    version: String,
}

impl Default for DefaultCompiler {
    fn default() -> Self {
        Self::new(CompilerOptions::default())
    }
}

impl DefaultCompiler {
    /// Construct with the given options.
    #[must_use]
    pub fn new(options: CompilerOptions) -> Self {
        Self {
            options,
            version: concat!("paramodel-compiler-", env!("CARGO_PKG_VERSION"), "+v0.1").to_owned(),
        }
    }
}

impl Compiler for DefaultCompiler {
    fn compile(&self, plan: &TestPlan) -> Result<ExecutionPlan, CompilationError> {
        compile_impl(self, plan)
    }

    fn validate(&self, plan: &TestPlan) -> Vec<CompilationDiagnostic> {
        let mut diags = Vec::new();
        gather_unsupported(plan, &mut diags);
        if let Err(e) = plan.validate() {
            diags.push(error(
                WarningCode::E001,
                format!("plan validation failed: {e}"),
                DiagnosticLocation::Plan,
            ));
        }
        diags
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn options(&self) -> &CompilerOptions {
        &self.options
    }
}

// ---------------------------------------------------------------------------
// Internal pipeline.
// ---------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "pipeline stages read in order; later slices split per-rule helpers out"
)]
fn compile_impl(
    compiler: &DefaultCompiler,
    plan:     &TestPlan,
) -> Result<ExecutionPlan, CompilationError> {
    let started_at = Timestamp::now();

    // ---- plan-level validation ----
    let mut errors: Vec<CompilationDiagnostic> = Vec::new();
    if let Err(e) = plan.validate() {
        errors.push(error(
            WarningCode::E001,
            format!("plan validation failed: {e}"),
            DiagnosticLocation::Plan,
        ));
    }

    // ---- unsupported-feature checks ----
    gather_unsupported(plan, &mut errors);

    if !errors.is_empty() {
        return Err(CompilationError::many(errors));
    }

    // ---- Stage 1 ----
    let enumerator = MixedRadixEnumerator::new(&plan.axes);
    let bsc        = BindingStateComputer::compute(plan);
    let trial_elements: BTreeSet<ElementName> = identify_trial_elements(plan, &bsc);

    // ---- Stage 3 Rules 1 + 2 + 7 (shared-only), Stage 4 linearisation
    //      all happen together since the graph is small and there's no
    //      coalescing to complicate remapping. ----
    let trial_count = enumerator.trial_count();
    let mut steps:   Vec<AtomicStep> = Vec::new();

    // Pre-compute step ids and keep them around for edge materialisation.
    let mut deploy_id_of:   BTreeMap<(ElementName, u64), StepId> = BTreeMap::new();
    let mut teardown_id_of: BTreeMap<(ElementName, u64), StepId> = BTreeMap::new();
    for t in 0..trial_count {
        for e in &plan.elements {
            deploy_id_of.insert(
                (e.name.clone(), t),
                StepId::new(format!("activate_{}_t{t}", e.name.as_str()))
                    .map_err(|err| CompilationError::single(internal_error(err)))?,
            );
            teardown_id_of.insert(
                (e.name.clone(), t),
                StepId::new(format!("deactivate_{}_t{t}", e.name.as_str()))
                    .map_err(|err| CompilationError::single(internal_error(err)))?,
            );
        }
    }

    let start_id = StepId::new("start").expect("sentinel id is valid");
    let end_id   = StepId::new("end").expect("sentinel id is valid");

    // Per-trial Deploy + Teardown with Shared-edge dependencies.
    for t in 0..trial_count {
        let offsets = enumerator.offsets(t);
        let trial_code = enumerator.trial_code(t);

        for e in &plan.elements {
            // Resolve this trial's configuration for `e`.
            let resolved = match resolve_configuration(plan, e, &offsets) {
                Ok(r) => r,
                Err(diag) => return Err(CompilationError::single(diag)),
            };

            // ---- Deploy ----
            let deploy_id = deploy_id_of[&(e.name.clone(), t)].clone();
            let teardown_id = teardown_id_of[&(e.name.clone(), t)].clone();

            let mut deploy_deps: Vec<StepId> = vec![start_id.clone()];
            // Rule 2 — Shared edges. Deploy(X) depends on Deploy(target)
            // for every Shared dependency on this element.
            for dep in &e.dependencies {
                if matches!(dep.relationship, RelationshipType::Shared) {
                    deploy_deps.push(deploy_id_of[&(dep.target.clone(), t)].clone());
                }
            }

            let deploy_header = StepHeader::builder()
                .id(deploy_id.clone())
                .depends_on(deploy_deps)
                .reason(format!("initial deploy of {} for trial {t}", e.name))
                .trial_index(u32::try_from(t).unwrap_or(u32::MAX))
                .trial_code(trial_code.clone())

                .resource_requirements(ResourceRequirements::none())
                .build();

            steps.push(AtomicStep::Deploy {
                header:                deploy_header,
                element:               e.name.clone(),
                instance_number:       u32::try_from(t).unwrap_or(u32::MAX),
                configuration:         resolved,
                max_concurrency:       e.max_concurrency,
                max_group_concurrency: e.max_group_concurrency,
                dedicated_to:          None,
            });

            // ---- Teardown ----
            let teardown_deps = vec![deploy_id.clone()];
            // Rule 2 — Shared edges. Teardown(X) is a dependency of
            // Teardown(target). In other words: target.teardown deps
            // on X.teardown. Handled below on the target's iteration,
            // so we only record the forward-edge contribution here.
            // The actual "teardown(X) → teardown(target)" edge is
            // expressed by adding `deploy_id(X,t) → teardown_id(target,t)`
            // to target's teardown deps, which we do in a post-pass.

            let teardown_header = StepHeader::builder()
                .id(teardown_id.clone())
                .depends_on(teardown_deps.clone())
                .reason(format!("teardown of {} for trial {t}", e.name))
                .trial_index(u32::try_from(t).unwrap_or(u32::MAX))
                .trial_code(trial_code.clone())

                .resource_requirements(ResourceRequirements::none())
                .build();
            let _ = teardown_deps;

            steps.push(AtomicStep::Teardown {
                header:            teardown_header,
                element:           e.name.clone(),
                instance_number:   u32::try_from(t).unwrap_or(u32::MAX),
                collect_artifacts: false,
            });
        }
    }

    // ---- Rule 2 post-pass: Shared / Linear / Lifeline ---------------------
    apply_rule2_edges(
        &mut steps,
        plan,
        trial_count,
        &deploy_id_of,
        &teardown_id_of,
        &trial_elements,
        &bsc,
    );

    // Rule 2 — Dedicated: materialise per-owner Y instances for every
    // Dedicated(Y) declared by an owner X.
    apply_dedicated_materialisation(
        &mut steps,
        plan,
        trial_count,
        &deploy_id_of,
        &teardown_id_of,
        &start_id,
    )
    .map_err(CompilationError::single)?;

    // Rule 2 — Exclusive: same-element consecutive-trial serialisation +
    // W002 detection for cross-prototype Exclusive on the same target
    // in the same trial. Must be warnings we *collect*, so the caller
    // sees them on success too. For slice A2 we bubble W002 warnings up
    // as hard errors because cross-prototype Exclusive isn't rewritten
    // into a valid graph shape yet.
    let mut exclusive_warnings: Vec<CompilationDiagnostic> = Vec::new();
    apply_exclusive_serialisation(
        &mut steps,
        plan,
        trial_count,
        &deploy_id_of,
        &teardown_id_of,
        &mut exclusive_warnings,
    );
    if !exclusive_warnings.is_empty() {
        return Err(CompilationError::many(exclusive_warnings));
    }

    // Lifeline: X with a Lifeline(Y) dependency has no Teardown — Y's
    // teardown collapses X's lifecycle. Remove X's teardown steps and
    // rewire any edges that targeted them onto Y's teardown.
    apply_lifeline_collapse(&mut steps, plan, trial_count, &teardown_id_of);

    // ---- Rule 3 — group coalescing ---------------------------------------
    apply_coalescing(
        &mut steps,
        plan,
        trial_count,
        &bsc,
        &trial_elements,
        &enumerator,
        &deploy_id_of,
        &teardown_id_of,
    );

    // ---- Rule 4 — trial notifications ------------------------------------
    apply_trial_notifications(
        &mut steps,
        plan,
        trial_count,
        &bsc,
        &trial_elements,
        &enumerator,
        &deploy_id_of,
        &teardown_id_of,
    )
    .map_err(CompilationError::single)?;

    // ---- Rule 2 / Rule 4 — Exclusive cross-trial rerouting ----------------
    apply_exclusive_rerouting(
        &mut steps,
        plan,
        trial_count,
        &trial_elements,
        &deploy_id_of,
        &teardown_id_of,
    );

    // ---- Rule 5 — health-check readiness gates ---------------------------
    apply_readiness_gates(&mut steps, plan).map_err(CompilationError::single)?;

    // ---- Start / End sentinels — Rule 7 ----
    // Start precedes every step with no other incoming edges; its own
    // depends_on is empty.
    let start_step = AtomicStep::Checkpoint {
        header: StepHeader::builder()
            .id(start_id)
            .reason("sentinel: graph start".to_owned())

            .resource_requirements(ResourceRequirements::none())
            .build(),
        checkpoint_id: CheckpointId::new("start").expect("reserved sentinel id"),
    };

    // Every step that is not depended on by any other step feeds into
    // End.
    let mut end_depends_on: Vec<StepId> = Vec::new();
    {
        let incoming: BTreeSet<&StepId> = steps
            .iter()
            .flat_map(|s| s.depends_on().iter())
            .collect();
        for s in &steps {
            if !incoming.contains(s.id()) {
                end_depends_on.push(s.id().clone());
            }
        }
    }
    let end_step = AtomicStep::Checkpoint {
        header: StepHeader::builder()
            .id(end_id)
            .depends_on(end_depends_on)
            .reason("sentinel: graph end".to_owned())

            .resource_requirements(ResourceRequirements::none())
            .build(),
        checkpoint_id: CheckpointId::new("end").expect("reserved sentinel id"),
    };

    steps.insert(0, start_step);
    steps.push(end_step);

    // ---- Rule 8 — transitive reduction -----------------------------------
    apply_transitive_reduction(&mut steps);

    let execution_graph = ExecutionGraph::new(steps).map_err(|e| {
        CompilationError::single(error(
            WarningCode::E003,
            format!("compiler invariant violated: {e}"),
            DiagnosticLocation::Plan,
        ))
    })?;

    // ---- ElementInstanceGraph (trivial — one instance per (element, trial)) ----
    let element_instance_graph =
        build_trivial_instance_graph(plan, trial_count, &enumerator, &bsc, &trial_elements);

    // ---- Metadata ----
    let completed_at = Timestamp::now();
    let compilation_duration = Duration::try_from(completed_at.duration_since(started_at))
        .unwrap_or(Duration::ZERO);
    let step_count    = u32::try_from(execution_graph.steps().len()).unwrap_or(u32::MAX);
    let barrier_count = u32::try_from(execution_graph.barriers().count()).unwrap_or(u32::MAX);
    let instance_count =
        u32::try_from(element_instance_graph.total_instances()).unwrap_or(u32::MAX);

    let metadata = ExecutionPlanMetadata::builder()
        .compiled_at(completed_at)
        .compilation_duration(compilation_duration)
        .compiler_version(compiler.version.clone())
        .optimization_level(plan.optimization_strategy)
        .trial_count(u32::try_from(trial_count).unwrap_or(u32::MAX))
        .step_count(step_count)
        .barrier_count(barrier_count)
        .element_instance_count(instance_count)

        .performance_metrics(PerformanceMetrics {
            critical_path_duration: None,
            total_duration:         None,
            maximum_parallelism:    1,
            average_parallelism:    1.0,
            speedup_factor:         1.0,
        })
        .build();

    let plan_fp = plan.fingerprint();
    Ok(ExecutionPlan::builder()
        .id(ExecutionPlanId::from_ulid(Ulid::from_parts(
            u64::try_from(completed_at.as_second().max(0)).unwrap_or(0),
            0,
        )))
        .source_plan_fingerprint(plan_fp)
        .source_plan_id(plan.id)
        .execution_graph(execution_graph)
        .element_instance_graph(element_instance_graph)
        .trial_ordering(plan.trial_ordering.clone())
        .trial_elements(trial_elements.into_iter().collect())
        .metadata(metadata)
        .build())
}

// ---------------------------------------------------------------------------
// Unsupported-feature detection.
// ---------------------------------------------------------------------------

fn gather_unsupported(plan: &TestPlan, out: &mut Vec<CompilationDiagnostic>) {
    // Every declared dependency must target an element the plan
    // also declares. Upstream plan validation doesn't catch this
    // consistently, so do it here — otherwise downstream edge
    // materialisation tries to look up a step id for a missing
    // target and panics.
    let declared: BTreeSet<&ElementName> =
        plan.elements.iter().map(|e| &e.name).collect();
    for e in &plan.elements {
        for dep in &e.dependencies {
            if !declared.contains(&dep.target) {
                out.push(error(
                    WarningCode::E001,
                    format!(
                        "element '{}' depends on '{}', which is not declared \
                         in this plan",
                        e.name, dep.target,
                    ),
                    DiagnosticLocation::Dependency {
                        source: e.name.as_str().to_owned(),
                        target: dep.target.as_str().to_owned(),
                    },
                ));
            }
        }
    }

    for e in &plan.elements {
        if matches!(e.shutdown_semantics, ShutdownSemantics::Command) {
            out.push(error(
                WarningCode::E002,
                format!(
                    "element '{}' has shutdown_semantics = Command; v0.1 compiler supports \
                     Service elements only",
                    e.name,
                ),
                DiagnosticLocation::Element {
                    name: e.name.as_str().to_owned(),
                },
            ));
        }
        // Health checks are now supported via Rule 5 (readiness gates).
        // All five relationship types are now supported. A
        // cross-prototype Exclusive collision on the same target
        // within the same trial scope produces W002 — see
        // `detect_exclusive_collisions`.
        // Token-based configuration entries require a token-resolution
        // layer that v0.1 doesn't ship.
        for (name, entry) in e.configuration.iter() {
            if entry.is_token() {
                out.push(error(
                    WarningCode::E002,
                    format!(
                        "element '{}' configuration parameter '{}' uses a token expression; \
                         token resolution is not yet wired up",
                        e.name, name,
                    ),
                    DiagnosticLocation::Parameter {
                        element:   e.name.as_str().to_owned(),
                        parameter: name.as_str().to_owned(),
                    },
                ));
            }
        }
    }
    for (coord, entry) in plan.bindings.iter() {
        if entry.is_token() {
            out.push(error(
                WarningCode::E002,
                format!(
                    "plan binding ({}, {}) uses a token expression; not yet supported",
                    coord.element, coord.parameter,
                ),
                DiagnosticLocation::Parameter {
                    element:   coord.element.as_str().to_owned(),
                    parameter: coord.parameter.as_str().to_owned(),
                },
            ));
        }
    }
    // Plan bindings are accepted only when literal — checked above.
    // Custom trial orderings aren't registered, so warn.
    if let paramodel_plan::TrialOrdering::Custom { name } = &plan.trial_ordering {
        out.push(error(
            WarningCode::E002,
            format!(
                "plan uses TrialOrdering::Custom {{ name: '{name}' }} but the v0.1 compiler has \
                 no custom-ordering registry",
            ),
            DiagnosticLocation::Plan,
        ));
    }
}

fn internal_error(err: impl std::fmt::Display) -> CompilationDiagnostic {
    error(
        WarningCode::E003,
        format!("compiler internal error: {err}"),
        DiagnosticLocation::Plan,
    )
}

// ---------------------------------------------------------------------------
// Rule 2 — dependency edge materialisation (Shared / Linear / Lifeline).
// ---------------------------------------------------------------------------

/// Walk every `(element, trial)` pair and materialise the edges each
/// dependency kind implies. Shared keeps the v0.1 behaviour;
/// Linear adds `deactivate(Y, T) → activate(X, T)`; Lifeline adds the
/// forward activate edge and defers the teardown rewrite to
/// [`apply_lifeline_collapse`].
fn apply_rule2_edges(
    steps:           &mut [AtomicStep],
    plan:            &TestPlan,
    trial_count:     u64,
    deploy_id_of:    &BTreeMap<(ElementName, u64), StepId>,
    teardown_id_of:  &BTreeMap<(ElementName, u64), StepId>,
    _trial_elements: &BTreeSet<ElementName>,
    bsc:             &BindingStateComputer,
) {
    for t in 0..trial_count {
        for e in &plan.elements {
            for dep in &e.dependencies {
                match dep.relationship {
                    RelationshipType::Shared => {
                        // Shared's deploy-side edge was added inline
                        // during step construction. Here we add the
                        // teardown-side edge: target.teardown depends
                        // on x.teardown.
                        let x_teardown = teardown_id_of[&(e.name.clone(), t)].clone();
                        let target_teardown =
                            teardown_id_of[&(dep.target.clone(), t)].clone();
                        add_dependency(steps, &target_teardown, &x_teardown);
                    }
                    RelationshipType::Linear => {
                        // SRD-0010 §S.1: the Linear edge is added only
                        // when X and Y share the same configuration
                        // group (same effective binding level). When
                        // they don't, their lifecycles are
                        // independent — adding the edge would wire a
                        // cycle through Rule 4's notifications
                        // (X's activate → Y's teardown → notify_end
                        // → X's teardown → X's activate).
                        if !bsc.same_group_for_elements(&e.name, &dep.target) {
                            continue;
                        }
                        let x_deploy = deploy_id_of[&(e.name.clone(), t)].clone();
                        let y_teardown =
                            teardown_id_of[&(dep.target.clone(), t)].clone();
                        add_dependency(steps, &x_deploy, &y_teardown);
                    }
                    RelationshipType::Lifeline => {
                        // Lifeline: activate(Y, T) → activate(X, T).
                        // X's teardown will be removed below; edges
                        // targeting it get remapped onto Y's teardown.
                        let x_deploy = deploy_id_of[&(e.name.clone(), t)].clone();
                        let y_deploy = deploy_id_of[&(dep.target.clone(), t)].clone();
                        add_dependency(steps, &x_deploy, &y_deploy);
                    }
                    RelationshipType::Dedicated | RelationshipType::Exclusive => {
                        // Rejected by `gather_unsupported` — unreachable.
                    }
                }
            }
        }
    }
}

/// Add `to_add` to `target_step.depends_on` if not already present.
fn add_dependency(steps: &mut [AtomicStep], target_step: &StepId, to_add: &StepId) {
    for step in steps.iter_mut() {
        if step.id() == target_step {
            let deps = depends_on_mut(step);
            if !deps.iter().any(|d| d == to_add) {
                deps.push(to_add.clone());
            }
            return;
        }
    }
}

/// Borrow the `depends_on` vec for any `AtomicStep` variant. All eight
/// variants share the same `StepHeader.depends_on` field.
const fn depends_on_mut(step: &mut AtomicStep) -> &mut Vec<StepId> {
    match step {
        AtomicStep::Deploy { header, .. }
        | AtomicStep::Teardown { header, .. }
        | AtomicStep::TrialStart { header, .. }
        | AtomicStep::TrialEnd { header, .. }
        | AtomicStep::Await { header, .. }
        | AtomicStep::SaveOutput { header, .. }
        | AtomicStep::Barrier { header, .. }
        | AtomicStep::Checkpoint { header, .. } => &mut header.depends_on,
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — Dedicated.
// ---------------------------------------------------------------------------

/// Generate per-owner `Y` instances for every `Dedicated(Y)` declared
/// by an owner `X`. Each owner `X` at trial `T` gets its own
/// `dedicated_{Y}_for_{X}_t{T}` deploy/teardown pair, wired between
/// `start` → `dedicated_deploy → X_deploy` and
/// `X_teardown → dedicated_teardown`.
///
/// Without Rule 3 coalescing, each `(X, T)` maps 1-to-1 to one
/// dedicated `Y` instance. Instance numbers are assigned from
/// `trial_count..` so they don't collide with `Y`'s standalone
/// instances at `0..trial_count`.
#[allow(
    clippy::too_many_lines,
    reason = "single pass over dedicated edges; split hurts readability"
)]
fn apply_dedicated_materialisation(
    steps:          &mut Vec<AtomicStep>,
    plan:           &TestPlan,
    trial_count:    u64,
    deploy_id_of:   &BTreeMap<(ElementName, u64), StepId>,
    teardown_id_of: &BTreeMap<(ElementName, u64), StepId>,
    start_id:       &StepId,
) -> Result<(), CompilationDiagnostic> {
    // Collect all dedicated edges: (owner, target).
    let mut dedicated_edges: Vec<(ElementName, ElementName)> = Vec::new();
    for e in &plan.elements {
        for dep in &e.dependencies {
            if matches!(dep.relationship, RelationshipType::Dedicated) {
                dedicated_edges.push((e.name.clone(), dep.target.clone()));
            }
        }
    }
    if dedicated_edges.is_empty() {
        return Ok(());
    }

    // Assign a stable slot per (target, owner) pair so instance
    // numbers are deterministic across trials.
    let mut slot_of: BTreeMap<(ElementName, ElementName), u32> = BTreeMap::new();
    for (owner, target) in &dedicated_edges {
        let next = u32::try_from(slot_of.len()).unwrap_or(u32::MAX);
        slot_of
            .entry((target.clone(), owner.clone()))
            .or_insert(next);
    }
    let base_instance =
        u32::try_from(trial_count).unwrap_or(u32::MAX).saturating_add(1);

    for (owner, target) in &dedicated_edges {
        let slot = slot_of[&(target.clone(), owner.clone())];
        for t in 0..trial_count {
            let offsets = plan
                .axes
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    // Reuse the shared resolver for bindings — for a
                    // dedicated Y, the configuration comes from Y's
                    // definition and any axis binding on Y.
                    let _ = (i, a);
                    0u32
                })
                .collect::<Vec<_>>();
            let _ = offsets; // unused stub — configuration below reuses resolve_configuration.

            let deploy_id = StepId::new(format!(
                "activate_dedicated_{}_for_{}_t{t}",
                target.as_str(),
                owner.as_str(),
            ))
            .map_err(internal_error)?;
            let teardown_id = StepId::new(format!(
                "deactivate_dedicated_{}_for_{}_t{t}",
                target.as_str(),
                owner.as_str(),
            ))
            .map_err(internal_error)?;

            let target_element = plan
                .elements
                .iter()
                .find(|e| &e.name == target)
                .ok_or_else(|| {
                    error(
                        WarningCode::E001,
                        format!("Dedicated target '{target}' is not declared"),
                        DiagnosticLocation::Dependency {
                            source: owner.as_str().to_owned(),
                            target: target.as_str().to_owned(),
                        },
                    )
                })?;
            let enumerator_offsets: Vec<u32> = plan
                .axes
                .iter()
                .enumerate()
                .map(|(axis_index, _axis)| {
                    // Reuse MixedRadixEnumerator ordering: call via
                    // re-computation at this trial index.
                    let _ = axis_index;
                    0
                })
                .collect();
            let _ = enumerator_offsets;
            let enumerator = MixedRadixEnumerator::new(&plan.axes);
            let offsets = enumerator.offsets(t);
            let trial_code = enumerator.trial_code(t);
            let resolved = resolve_configuration(plan, target_element, &offsets)?;

            let trial_index = u32::try_from(t).unwrap_or(u32::MAX);
            let dedicated_instance = base_instance
                .saturating_add(slot.saturating_mul(
                    u32::try_from(trial_count).unwrap_or(u32::MAX),
                ))
                .saturating_add(trial_index);

            // Deploy: depends on start; the owner's deploy depends on
            // this dedicated deploy (wired below).
            let deploy_header = StepHeader::builder()
                .id(deploy_id.clone())
                .depends_on(vec![start_id.clone()])
                .reason(format!(
                    "dedicated deploy of {target} for owner {owner} at trial {t}",
                ))
                .trial_index(trial_index)
                .trial_code(trial_code.clone())
                .resource_requirements(ResourceRequirements::none())
                .build();
            steps.push(AtomicStep::Deploy {
                header:                deploy_header,
                element:               target.clone(),
                instance_number:       dedicated_instance,
                configuration:         resolved,
                max_concurrency:       target_element.max_concurrency,
                max_group_concurrency: target_element.max_group_concurrency,
                dedicated_to:          Some(owner.clone()),
            });

            // Teardown: depends on owner's teardown (so owner tears
            // down first).
            let owner_teardown = teardown_id_of[&(owner.clone(), t)].clone();
            let teardown_header = StepHeader::builder()
                .id(teardown_id.clone())
                .depends_on(vec![owner_teardown])
                .reason(format!(
                    "dedicated teardown of {target} for owner {owner} at trial {t}",
                ))
                .trial_index(trial_index)
                .trial_code(trial_code.clone())
                .resource_requirements(ResourceRequirements::none())
                .build();
            steps.push(AtomicStep::Teardown {
                header:            teardown_header,
                element:           target.clone(),
                instance_number:   dedicated_instance,
                collect_artifacts: false,
            });

            // Owner's deploy depends on dedicated deploy.
            let owner_deploy = deploy_id_of[&(owner.clone(), t)].clone();
            add_dependency(steps, &owner_deploy, &deploy_id);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rule 2 — Exclusive.
// ---------------------------------------------------------------------------

/// For every Exclusive dependent `X → Y`:
/// - add `deactivate(X, T_i) → activate(X, T_{i+1})` across consecutive
///   trials (same-element serialisation).
/// - detect W002 when two distinct prototypes exclusively depend on
///   the same target within the same trial.
///
/// Cross-prototype serialisation across trials (different X and Z both
/// exclusive on Y at adjacent trials) is deferred to Slice B alongside
/// Rule 3's group coalescing — without group boundaries the "next
/// trial requiring Z" resolution is trivial but the graph shape is
/// over-constrained without the notification-reroute pass.
fn apply_exclusive_serialisation(
    steps:          &mut [AtomicStep],
    plan:           &TestPlan,
    trial_count:    u64,
    deploy_id_of:   &BTreeMap<(ElementName, u64), StepId>,
    teardown_id_of: &BTreeMap<(ElementName, u64), StepId>,
    warnings:       &mut Vec<CompilationDiagnostic>,
) {
    // Collect exclusive edges.
    let mut exclusive: Vec<(ElementName, ElementName)> = Vec::new();
    for e in &plan.elements {
        for dep in &e.dependencies {
            if matches!(dep.relationship, RelationshipType::Exclusive) {
                exclusive.push((e.name.clone(), dep.target.clone()));
                // Shared edges (activate/deactivate) for exclusive —
                // exclusive implies shared+serialisation, so add the
                // shared-style edges inline here.
                for t in 0..trial_count {
                    let x_deploy = deploy_id_of[&(e.name.clone(), t)].clone();
                    let y_deploy = deploy_id_of[&(dep.target.clone(), t)].clone();
                    add_dependency(steps, &x_deploy, &y_deploy);
                    let x_teardown = teardown_id_of[&(e.name.clone(), t)].clone();
                    let y_teardown = teardown_id_of[&(dep.target.clone(), t)].clone();
                    add_dependency(steps, &y_teardown, &x_teardown);
                }
            }
        }
    }

    // Same-element cross-trial serialisation.
    for (x, _y) in &exclusive {
        for t in 0..trial_count.saturating_sub(1) {
            let x_teardown_ti = teardown_id_of[&(x.clone(), t)].clone();
            let x_deploy_tnext = deploy_id_of[&(x.clone(), t + 1)].clone();
            add_dependency(steps, &x_deploy_tnext, &x_teardown_ti);
        }
    }

    // W002 detection: two distinct prototypes X and Z both exclusively
    // depend on the same target Y within the same trial.
    let mut by_target: BTreeMap<ElementName, BTreeSet<ElementName>> = BTreeMap::new();
    for (x, y) in &exclusive {
        by_target.entry(y.clone()).or_default().insert(x.clone());
    }
    for (target, dependents) in &by_target {
        if dependents.len() > 1 {
            let mut names: Vec<&ElementName> = dependents.iter().collect();
            names.sort();
            warnings.push(error(
                WarningCode::W002,
                format!(
                    "elements [{}] all Exclusive-depend on '{target}' within the same \
                     trial scope; cross-prototype Exclusive rerouting is not yet \
                     implemented — split the exclusions across distinct targets or \
                     use Dedicated",
                    names
                        .iter()
                        .map(|n| format!("'{n}'"))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                DiagnosticLocation::Element {
                    name: target.as_str().to_owned(),
                },
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2 / Rule 4 — Exclusive cross-trial rerouting.
// ---------------------------------------------------------------------------

/// For trial-element Exclusive dependencies, reroute the direct
/// `deactivate(X, T_i) → activate(X, T_{i+1})` serialisation edge
/// through the notification boundaries:
/// `deactivate(X, T_i) → notify_trial_end(T_i) → notify_trial_start(T_{i+1}) →
/// activate(X, T_{i+1})`.
///
/// The first and last hops already exist from Rule 4; this pass adds
/// the `notify_end → notify_start` bridge and removes the direct
/// cross-trial Exclusive edge.
///
/// Non-trial Exclusive edges are left alone — rerouting would invert
/// the semantic (see SRD §Rule 4 non-trial discussion).
fn apply_exclusive_rerouting(
    steps:          &mut [AtomicStep],
    plan:           &TestPlan,
    trial_count:    u64,
    trial_elements: &BTreeSet<ElementName>,
    deploy_id_of:   &BTreeMap<(ElementName, u64), StepId>,
    teardown_id_of: &BTreeMap<(ElementName, u64), StepId>,
) {
    // Did any trial element declare an Exclusive dep?
    let trial_has_exclusive = plan.elements.iter().any(|e| {
        trial_elements.contains(&e.name)
            && e.dependencies
                .iter()
                .any(|d| matches!(d.relationship, RelationshipType::Exclusive))
    });
    if !trial_has_exclusive {
        return;
    }

    let live_ids: BTreeSet<StepId> = steps.iter().map(|s| s.id().clone()).collect();

    // Bridge every consecutive trial pair's notifications.
    for i in 0..trial_count.saturating_sub(1) {
        let Ok(end_id) = StepId::new(format!("notify_trial_end_t{i}")) else {
            continue;
        };
        let Ok(start_id) = StepId::new(format!("notify_trial_start_t{}", i + 1))
        else {
            continue;
        };
        if !live_ids.contains(&end_id) || !live_ids.contains(&start_id) {
            continue;
        }
        add_dependency(steps, &start_id, &end_id);
    }

    // Remove direct same-element cross-trial Exclusive edges for trial
    // elements — they're now mediated by the bridge above.
    for e in &plan.elements {
        if !trial_elements.contains(&e.name) {
            continue;
        }
        let has_excl = e
            .dependencies
            .iter()
            .any(|d| matches!(d.relationship, RelationshipType::Exclusive));
        if !has_excl {
            continue;
        }
        for i in 0..trial_count.saturating_sub(1) {
            let from_teardown = teardown_id_of[&(e.name.clone(), i)].clone();
            let to_deploy = deploy_id_of[&(e.name.clone(), i + 1)].clone();
            remove_dependency(steps, &to_deploy, &from_teardown);
        }
    }
}

/// Remove `to_remove` from `target_step.depends_on` if present.
fn remove_dependency(steps: &mut [AtomicStep], target: &StepId, to_remove: &StepId) {
    for step in steps.iter_mut() {
        if step.id() == target {
            let deps = depends_on_mut(step);
            deps.retain(|d| d != to_remove);
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — Lifeline collapse.
// ---------------------------------------------------------------------------

/// Lifeline: X has a lifeline to Y. Y's deactivation tears down X, so
/// X has no teardown of its own. Per-trial, remove `teardown(X, T)`
/// and remap any edges that referenced it onto `teardown(Y, T)`.
///
/// Lifeline clusters (X → Y → Z all lifeline-linked) collapse onto
/// the root (the element no lifeline names as its target).
fn apply_lifeline_collapse(
    steps:          &mut Vec<AtomicStep>,
    plan:           &TestPlan,
    trial_count:    u64,
    teardown_id_of: &BTreeMap<(ElementName, u64), StepId>,
) {
    // Build lifeline-root resolution: for each element that has a
    // lifeline dependency, walk the chain until we reach an element
    // with no lifeline dependency — that's the root.
    let mut lifeline_root: BTreeMap<ElementName, ElementName> = BTreeMap::new();
    for e in &plan.elements {
        if let Some(root) = lifeline_root_of(plan, &e.name)
            && root != e.name
        {
            lifeline_root.insert(e.name.clone(), root);
        }
    }
    if lifeline_root.is_empty() {
        return;
    }

    // Build remap table: teardown(X, T) → teardown(root, T).
    let mut remap: BTreeMap<StepId, StepId> = BTreeMap::new();
    for (x, root) in &lifeline_root {
        for t in 0..trial_count {
            if let (Some(x_td), Some(root_td)) = (
                teardown_id_of.get(&(x.clone(), t)),
                teardown_id_of.get(&(root.clone(), t)),
            ) {
                remap.insert(x_td.clone(), root_td.clone());
            }
        }
    }

    // Rewrite every step's depends_on through the remap table.
    for step in steps.iter_mut() {
        let deps = depends_on_mut(step);
        let mut seen: BTreeSet<StepId> = BTreeSet::new();
        let mut rewritten = Vec::with_capacity(deps.len());
        for d in deps.drain(..) {
            let dep = remap.get(&d).cloned().unwrap_or(d);
            if seen.insert(dep.clone()) {
                rewritten.push(dep);
            }
        }
        *deps = rewritten;
    }

    // Remove the now-orphaned teardown steps for lifeline-dependent
    // elements.
    let to_remove: BTreeSet<StepId> = remap.keys().cloned().collect();
    steps.retain(|s| !to_remove.contains(s.id()));
}

/// Walk lifeline dependencies from `element` until we reach an
/// element that does not declare a lifeline dependency. Returns that
/// root. Returns the input unchanged if the element has no lifeline
/// dependency. Returns `None` if a cycle is detected.
fn lifeline_root_of(plan: &TestPlan, element: &ElementName) -> Option<ElementName> {
    let mut seen: BTreeSet<ElementName> = BTreeSet::new();
    let mut current = element.clone();
    loop {
        if !seen.insert(current.clone()) {
            return None;
        }
        let e = plan.elements.iter().find(|x| x.name == current)?;
        let mut lifelined_to: Option<ElementName> = None;
        for dep in &e.dependencies {
            if matches!(dep.relationship, RelationshipType::Lifeline) {
                lifelined_to = Some(dep.target.clone());
                break;
            }
        }
        match lifelined_to {
            Some(next) => current = next,
            None => return Some(current),
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3 — group coalescing.
// ---------------------------------------------------------------------------

/// Number of trials that share offsets at ranks `[0..level)` — the
/// group size for a non-trial element with that effective binding
/// level.
fn group_size_at(enumerator: &MixedRadixEnumerator, level: u32) -> u64 {
    let n = u32::try_from(enumerator.axis_count()).unwrap_or(u32::MAX);
    if level == 0 {
        return enumerator.trial_count();
    }
    if level >= n {
        return 1;
    }
    enumerator.strides()[level as usize - 1]
}

/// Fold consecutive trials of each non-trial element into a single
/// activate/deactivate when the element's effective binding level
/// lets it. Trial elements are never coalesced. Dedicated target
/// instances (`activate_dedicated_…`) are not touched in this slice —
/// they don't appear in `deploy_id_of` / `teardown_id_of`.
///
/// For each coalesced group at trials `[first..=last]`:
/// - Keep `activate(E, first)` as the group's activation.
/// - Keep `deactivate(E, last)` as the group's deactivation.
/// - Every other `activate(E, t)` and `deactivate(E, t)` is removed;
///   its `depends_on` is folded onto the surviving node, and every
///   edge elsewhere in the graph that referenced the removed node is
///   remapped onto the surviving one.
#[allow(
    clippy::too_many_arguments,
    reason = "pipeline helper; packaging into a struct here costs more than it saves"
)]
fn apply_coalescing(
    steps:          &mut Vec<AtomicStep>,
    plan:           &TestPlan,
    trial_count:    u64,
    bsc:            &BindingStateComputer,
    trial_elements: &BTreeSet<ElementName>,
    enumerator:     &MixedRadixEnumerator,
    deploy_id_of:   &BTreeMap<(ElementName, u64), StepId>,
    teardown_id_of: &BTreeMap<(ElementName, u64), StepId>,
) {
    if trial_count == 0 {
        return;
    }
    let mut remap: BTreeMap<StepId, StepId> = BTreeMap::new();
    let mut to_remove: BTreeSet<StepId> = BTreeSet::new();

    for e in &plan.elements {
        if trial_elements.contains(&e.name) {
            continue;
        }
        let level = bsc.effective_level(&e.name);
        let group_size = group_size_at(enumerator, level);
        if group_size <= 1 {
            continue;
        }

        let mut t = 0u64;
        while t < trial_count {
            let first = t;
            let last = (first + group_size - 1).min(trial_count - 1);

            let kept_activate = deploy_id_of[&(e.name.clone(), first)].clone();
            let kept_deactivate = teardown_id_of[&(e.name.clone(), last)].clone();

            // Removed activates coalesce onto `first`.
            for tt in (first + 1)..=last {
                let removed = deploy_id_of[&(e.name.clone(), tt)].clone();
                remap.insert(removed.clone(), kept_activate.clone());
                to_remove.insert(removed);
            }
            // Removed deactivates coalesce onto `last`.
            for tt in first..last {
                let removed = teardown_id_of[&(e.name.clone(), tt)].clone();
                remap.insert(removed.clone(), kept_deactivate.clone());
                to_remove.insert(removed);
            }

            t = last + 1;
        }
    }

    if to_remove.is_empty() {
        return;
    }

    // Gather deps from removed steps so we can fold them onto the
    // surviving node before dropping the removed ones.
    let mut extra_deps: BTreeMap<StepId, Vec<StepId>> = BTreeMap::new();
    for step in steps.iter() {
        if to_remove.contains(step.id()) {
            let kept = remap[step.id()].clone();
            extra_deps
                .entry(kept)
                .or_default()
                .extend(step.depends_on().iter().cloned());
        }
    }

    steps.retain(|s| !to_remove.contains(s.id()));

    // Remap surviving steps' dependencies and absorb the folded-in
    // deps for kept nodes.
    for step in steps.iter_mut() {
        let step_id = step.id().clone();
        let extras = extra_deps.remove(&step_id).unwrap_or_default();
        let deps = depends_on_mut(step);
        // Apply remap to existing deps.
        for d in deps.iter_mut() {
            if let Some(kept) = remap.get(d) {
                *d = kept.clone();
            }
        }
        // Append folded-in deps (remapped too).
        for e in extras {
            let mapped = remap.get(&e).cloned().unwrap_or(e);
            deps.push(mapped);
        }
        // Remove self-loops introduced by remapping.
        deps.retain(|d| d != &step_id);
        // Dedupe while preserving order.
        let mut seen: BTreeSet<StepId> = BTreeSet::new();
        deps.retain(|d| seen.insert(d.clone()));
    }
}

// ---------------------------------------------------------------------------
// Rule 4 — trial notifications.
// ---------------------------------------------------------------------------

/// Deterministic `TrialId` for `(plan, trial_index)`. Compilation must
/// be reproducible, so we derive the id from the plan's id bits plus
/// the trial index rather than minting a fresh ULID.
fn trial_id_for(plan: &TestPlan, trial_index: u64) -> TrialId {
    let plan_ulid = plan.id.as_ulid();
    let random = plan_ulid.random() ^ u128::from(trial_index);
    TrialId::from_ulid(Ulid::from_parts(plan_ulid.timestamp_ms(), random))
}

/// Which activate node is currently "live" for `(element, trial)`
/// after coalescing. Trial elements are per-trial; non-trial elements
/// resolve to their group's first-trial activate.
fn current_activate_id(
    element:        &ElementName,
    trial:          u64,
    trial_elements: &BTreeSet<ElementName>,
    bsc:            &BindingStateComputer,
    enumerator:     &MixedRadixEnumerator,
    deploy_id_of:   &BTreeMap<(ElementName, u64), StepId>,
) -> Option<StepId> {
    if trial_elements.contains(element) {
        return deploy_id_of.get(&(element.clone(), trial)).cloned();
    }
    let level = bsc.effective_level(element);
    let group_size = group_size_at(enumerator, level);
    let group_first = (trial / group_size) * group_size;
    deploy_id_of.get(&(element.clone(), group_first)).cloned()
}

/// Which deactivate node is currently "live" for `(element, trial)`.
fn current_teardown_id(
    element:        &ElementName,
    trial:          u64,
    trial_elements: &BTreeSet<ElementName>,
    bsc:            &BindingStateComputer,
    enumerator:     &MixedRadixEnumerator,
    trial_count:    u64,
    teardown_id_of: &BTreeMap<(ElementName, u64), StepId>,
) -> Option<StepId> {
    if trial_elements.contains(element) {
        return teardown_id_of.get(&(element.clone(), trial)).cloned();
    }
    let level = bsc.effective_level(element);
    let group_size = group_size_at(enumerator, level);
    let group_first = (trial / group_size) * group_size;
    let group_last = (group_first + group_size - 1).min(trial_count - 1);
    teardown_id_of.get(&(element.clone(), group_last)).cloned()
}

/// Insert per-trial `TrialStart` / `TrialEnd` nodes and wire:
/// - `TrialStart(T)` depends on every non-trial element's current
///   activation.
/// - Each trial-element activate for `T` depends on `TrialStart(T)`.
/// - `TrialEnd(T)` depends on every trial-element deactivation for `T`.
/// - Each coalesced non-trial deactivation depends on `TrialEnd(T)`
///   for every trial `T` covered by its outgoing group.
#[allow(
    clippy::too_many_arguments,
    reason = "pipeline helper; packaging into a struct here costs more than it saves"
)]
#[allow(
    clippy::too_many_lines,
    reason = "single-purpose notification pass"
)]
fn apply_trial_notifications(
    steps:          &mut Vec<AtomicStep>,
    plan:           &TestPlan,
    trial_count:    u64,
    bsc:            &BindingStateComputer,
    trial_elements: &BTreeSet<ElementName>,
    enumerator:     &MixedRadixEnumerator,
    deploy_id_of:   &BTreeMap<(ElementName, u64), StepId>,
    teardown_id_of: &BTreeMap<(ElementName, u64), StepId>,
) -> Result<(), CompilationDiagnostic> {
    // Split plan elements into trial / non-trial. Non-trial elements
    // receive `on_trial_starting` / `on_trial_ending` hooks.
    let non_trial_elements: Vec<&Element> = plan
        .elements
        .iter()
        .filter(|e| !trial_elements.contains(&e.name))
        .collect();
    let trial_element_list: Vec<&Element> = plan
        .elements
        .iter()
        .filter(|e| trial_elements.contains(&e.name))
        .collect();

    // Notifications exist to bridge non-trial elements to trials.
    // With no trial elements in the plan there are no trial boundaries
    // to signal — skip Rule 4 entirely.
    if trial_element_list.is_empty() {
        return Ok(());
    }

    // Lifeline and coalescing can have removed steps; we must only
    // reference live ids in the notification wiring.
    let live_ids: BTreeSet<StepId> =
        steps.iter().map(|s| s.id().clone()).collect();

    let trial_code_ = |t: u64| -> String { enumerator.trial_code(t) };

    for t in 0..trial_count {
        let trial_id = trial_id_for(plan, t);
        let code = trial_code_(t);
        let trial_index = u32::try_from(t).unwrap_or(u32::MAX);

        // TrialStart: depends on every non-trial element's current
        // activation.
        let start_step_id = StepId::new(format!("notify_trial_start_t{t}"))
            .map_err(internal_error)?;
        let mut start_deps: Vec<StepId> = Vec::new();
        for e in &non_trial_elements {
            if let Some(a) = current_activate_id(
                &e.name,
                t,
                trial_elements,
                bsc,
                enumerator,
                deploy_id_of,
            ) && live_ids.contains(&a)
            {
                start_deps.push(a);
            }
        }
        dedupe_in_place(&mut start_deps);
        let non_trial_names: Vec<ElementName> =
            non_trial_elements.iter().map(|e| e.name.clone()).collect();
        steps.push(AtomicStep::TrialStart {
            header:        StepHeader::builder()
                .id(start_step_id.clone())
                .depends_on(start_deps)
                .reason(format!("notify trial start for trial {t}"))
                .trial_index(trial_index)
                .trial_code(code.clone())
                .resource_requirements(ResourceRequirements::none())
                .build(),
            trial_id,
            element_names: non_trial_names,
        });

        // Every trial-element activate for this trial depends on
        // TrialStart.
        for e in &trial_element_list {
            if let Some(a) =
                deploy_id_of.get(&(e.name.clone(), t)).cloned()
            {
                add_dependency(steps, &a, &start_step_id);
            }
        }

        // TrialEnd: depends on every trial-element deactivation for T.
        // Lifeline may have removed some teardowns; skip those.
        let end_step_id = StepId::new(format!("notify_trial_end_t{t}"))
            .map_err(internal_error)?;
        let mut end_deps: Vec<StepId> = Vec::new();
        for e in &trial_element_list {
            if let Some(d) = teardown_id_of.get(&(e.name.clone(), t)).cloned()
                && live_ids.contains(&d)
            {
                end_deps.push(d);
            }
        }
        dedupe_in_place(&mut end_deps);
        let trial_names: Vec<ElementName> =
            trial_element_list.iter().map(|e| e.name.clone()).collect();
        steps.push(AtomicStep::TrialEnd {
            header:          StepHeader::builder()
                .id(end_step_id.clone())
                .depends_on(end_deps)
                .reason(format!("notify trial end for trial {t}"))
                .trial_index(trial_index)
                .trial_code(code.clone())
                .resource_requirements(ResourceRequirements::none())
                .build(),
            trial_id,
            element_names:   trial_names,
            shutdown_reason: ShutdownReason::Normal,
        });

        // Every non-trial element's current teardown depends on
        // TrialEnd(t). Deduped across multiple trials of the same
        // group later via add_dependency's internal dedupe.
        // Skip teardowns removed by Lifeline.
        for e in &non_trial_elements {
            if let Some(d) = current_teardown_id(
                &e.name,
                t,
                trial_elements,
                bsc,
                enumerator,
                trial_count,
                teardown_id_of,
            ) && live_ids.contains(&d)
            {
                add_dependency(steps, &d, &end_step_id);
            }
        }
    }

    Ok(())
}

/// Dedupe a `Vec<StepId>` while preserving first-seen order.
fn dedupe_in_place(v: &mut Vec<StepId>) {
    let mut seen: BTreeSet<StepId> = BTreeSet::new();
    v.retain(|x| seen.insert(x.clone()));
}

// ---------------------------------------------------------------------------
// Rule 5 — health-check readiness gates.
// ---------------------------------------------------------------------------

/// For every `Deploy(E)` where `E` has a `HealthCheckSpec`, insert a
/// `Barrier { kind: ElementReady }` between the deploy and every step
/// that depended on the deploy (excluding the element's own teardown —
/// the direct edge must remain so teardown can still fire if the gate
/// never passes). Dependents of the deploy are rewritten to depend on
/// the gate instead.
fn apply_readiness_gates(
    steps: &mut Vec<AtomicStep>,
    plan:  &TestPlan,
) -> Result<(), CompilationDiagnostic> {
    // Collect elements with a health check.
    let gated: BTreeSet<ElementName> = plan
        .elements
        .iter()
        .filter(|e| e.health_check.is_some())
        .map(|e| e.name.clone())
        .collect();
    if gated.is_empty() {
        return Ok(());
    }

    // Find every Deploy step whose element is gated.
    let deploy_ids: Vec<(StepId, ElementName, Option<u32>, Option<String>)> = steps
        .iter()
        .filter_map(|s| {
            if let AtomicStep::Deploy {
                header,
                element,
                ..
            } = s
            {
                if gated.contains(element) {
                    Some((
                        header.id.clone(),
                        element.clone(),
                        header.trial_index,
                        header.trial_code.clone(),
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    if deploy_ids.is_empty() {
        return Ok(());
    }

    // Insert a gate per gated deploy; record the (deploy, gate, element)
    // triples.
    let mut gate_ids: BTreeMap<StepId, StepId> = BTreeMap::new();
    let mut gate_steps: Vec<AtomicStep> = Vec::new();
    let mut teardown_exemptions: BTreeSet<StepId> = BTreeSet::new();
    for (deploy_id, element, trial_index, trial_code) in &deploy_ids {
        let gate_step_id = StepId::new(format!(
            "readiness_{}_{}",
            element.as_str(),
            deploy_id.as_str(),
        ))
        .map_err(internal_error)?;
        let barrier_id = BarrierId::new(format!(
            "ready_{}",
            deploy_id.as_str(),
        ))
        .map_err(internal_error)?;

        let header = StepHeader::builder()
            .id(gate_step_id.clone())
            .depends_on(vec![deploy_id.clone()])
            .reason(format!("readiness gate for {element}"))
            .maybe_trial_index(*trial_index)
            .maybe_trial_code(trial_code.clone())
            .resource_requirements(ResourceRequirements::none())
            .build();
        gate_steps.push(AtomicStep::Barrier {
            header,
            barrier_id,
            barrier_kind:   BarrierKind::ElementReady,
            timeout:        None,
            timeout_action: TimeoutAction::FailFast,
        });
        gate_ids.insert(deploy_id.clone(), gate_step_id.clone());

        // Find the element's teardown for this trial (matching
        // instance number via the deploy step). Any step whose
        // depends_on references `deploy_id` AND is *not* that teardown
        // should be rewritten to depend on the gate instead.
        //
        // The teardown's trial_index matches the deploy's trial_index
        // and element, so we can find it by walking steps.
        for s in steps.iter() {
            if let AtomicStep::Teardown {
                header: td_header,
                element: td_element,
                ..
            } = s
                && td_element == element
                && td_header.trial_index == *trial_index
            {
                teardown_exemptions.insert(td_header.id.clone());
            }
        }
    }

    // Rewrite dependents: any step whose depends_on references a
    // gated deploy id — except the element's own teardown(s) — now
    // depends on the gate.
    for step in steps.iter_mut() {
        let step_id = step.id().clone();
        let is_exempt = teardown_exemptions.contains(&step_id);
        let deps = depends_on_mut(step);
        for d in deps.iter_mut() {
            if is_exempt {
                continue;
            }
            if let Some(gate) = gate_ids.get(d) {
                *d = gate.clone();
            }
        }
        dedupe_in_place(deps);
    }

    steps.extend(gate_steps);
    Ok(())
}

// ---------------------------------------------------------------------------
// Rule 8 — transitive reduction.
// ---------------------------------------------------------------------------

/// Remove every direct edge `N → S` where `S` is reachable from `N`
/// via some other successor of `N`. Reachability is preserved; the
/// edge set shrinks to the minimum required to express the ordering.
///
/// Complexity: `O(V × (V + E))` worst case (a BFS per edge). Fine
/// for the plan sizes the SRD anticipates.
fn apply_transitive_reduction(steps: &mut [AtomicStep]) {
    // Build an id → index map and an adjacency list keyed by id.
    let id_to_idx: BTreeMap<StepId, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id().clone(), i))
        .collect();
    // Outgoing adjacency: who depends on me?
    let mut outgoing: Vec<Vec<usize>> = vec![Vec::new(); steps.len()];
    for (i, s) in steps.iter().enumerate() {
        for d in s.depends_on() {
            if let Some(&src) = id_to_idx.get(d) {
                outgoing[src].push(i);
            }
        }
    }

    // For every node N, for every direct successor S, check whether
    // S is reachable from N via any *other* successor. If so, the
    // direct edge is redundant.
    let mut redundant: BTreeSet<(usize, usize)> = BTreeSet::new();
    for n in 0..steps.len() {
        let succs = outgoing[n].clone();
        for &s in &succs {
            // BFS from every other successor; skip the direct edge
            // (n, s).
            let mut visited: BTreeSet<usize> = BTreeSet::new();
            let mut stack: Vec<usize> = succs
                .iter()
                .copied()
                .filter(|&x| x != s)
                .collect();
            while let Some(cur) = stack.pop() {
                if !visited.insert(cur) {
                    continue;
                }
                if cur == s {
                    redundant.insert((n, s));
                    break;
                }
                for &next in &outgoing[cur] {
                    if !visited.contains(&next) {
                        stack.push(next);
                    }
                }
            }
        }
    }

    if redundant.is_empty() {
        return;
    }

    // Drop the redundant `n → s` edges. They're stored on `s`'s
    // depends_on as `n`.
    let idx_to_id: Vec<StepId> = steps.iter().map(|s| s.id().clone()).collect();
    for (n_idx, s_idx) in redundant {
        let n_id = idx_to_id[n_idx].clone();
        let deps = depends_on_mut(&mut steps[s_idx]);
        deps.retain(|d| d != &n_id);
    }
}

// ---------------------------------------------------------------------------
// Configuration resolution.
// ---------------------------------------------------------------------------

fn resolve_configuration(
    plan:    &TestPlan,
    element: &Element,
    offsets: &[u32],
) -> Result<ResolvedConfiguration, CompilationDiagnostic> {
    let mut out = ResolvedConfiguration::new();
    for parameter in &element.parameters {
        let pname: &ParameterName = parameter.name();

        // Step 1 — axis.
        if let Some((axis_index, axis)) = plan
            .axes
            .iter()
            .enumerate()
            .find(|(_, a)| a.target.element == element.name && a.target.parameter == *pname)
        {
            let offset = offsets.get(axis_index).copied().unwrap_or(0) as usize;
            if let Some(v) = axis.values.get(offset) {
                out.insert(pname.clone(), v.clone());
                continue;
            }
        }
        // Step 2 — plan binding.
        let coord = paramodel_plan::ElementParameterRef::new(
            element.name.clone(),
            pname.clone(),
        );
        if let Some(ConfigEntry::Literal { value }) = plan.bindings.get(&coord) {
            out.insert(pname.clone(), value.clone());
            continue;
        }
        // Step 3 — element configuration.
        if let Some(ConfigEntry::Literal { value }) = element.configuration.get(pname) {
            out.insert(pname.clone(), value.clone());
            continue;
        }
        // Step 4 — parameter default.
        if let Some(default) = parameter.default() {
            out.insert(pname.clone(), default);
            continue;
        }
        // Step 5 — error.
        return Err(error(
            WarningCode::E001,
            format!(
                "parameter '{}' on element '{}' has no binding source (no axis, no plan \
                 binding, no element configuration, no default)",
                pname, element.name,
            ),
            DiagnosticLocation::Parameter {
                element:   element.name.as_str().to_owned(),
                parameter: pname.as_str().to_owned(),
            },
        ));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Instance graph — trivial v0.1 shape.
// ---------------------------------------------------------------------------

fn build_trivial_instance_graph(
    plan:            &TestPlan,
    trial_count:     u64,
    enumerator:      &MixedRadixEnumerator,
    bsc:             &BindingStateComputer,
    trial_elements:  &BTreeSet<ElementName>,
) -> ElementInstanceGraph {
    let mut instances = Vec::new();
    let mut edges     = Vec::new();

    for t in 0..trial_count {
        let offsets = enumerator.offsets(t);
        let code    = enumerator.trial_code(t);
        for element in &plan.elements {
            let bindings = resolve_bindings_map(plan, element, &offsets);
            let id = InstanceId::from_parts(&element.name, u32::try_from(t).unwrap_or(u32::MAX));
            let scope = if trial_elements.contains(&element.name) {
                InstanceScope::Trial
            } else if bsc.effective_level(&element.name) == 0 {
                InstanceScope::Study
            } else {
                InstanceScope::Trial
            };
            instances.push(
                ElementInstance::builder()
                    .id(id.clone())
                    .element(element.name.clone())
                    .instance_number(u32::try_from(t).unwrap_or(u32::MAX))
                    .bindings(bindings)
                    .group_level(0)
                    .trial_code(code.clone())
                    .scope(scope)
                    .build(),
            );
        }
        // Shared / Linear / Lifeline edges in the instance graph.
        // Dedicated and Exclusive require instance multiplication and
        // land in a later slice.
        for element in &plan.elements {
            let src_id = InstanceId::from_parts(&element.name, u32::try_from(t).unwrap_or(u32::MAX));
            for dep in &element.dependencies {
                if !matches!(
                    dep.relationship,
                    RelationshipType::Shared
                        | RelationshipType::Linear
                        | RelationshipType::Lifeline
                ) {
                    continue;
                }
                let tgt_id = InstanceId::from_parts(&dep.target, u32::try_from(t).unwrap_or(u32::MAX));
                edges.push(paramodel_plan::InstanceDependency {
                    source:       src_id.clone(),
                    target:       tgt_id,
                    relationship: dep.relationship,
                });
            }
        }
    }

    ElementInstanceGraph::builder()
        .instances(instances)
        .edges(edges)
        .build()
}

fn resolve_bindings_map(
    plan:    &TestPlan,
    element: &Element,
    offsets: &[u32],
) -> BTreeMap<ParameterName, Value> {
    let mut out = BTreeMap::new();
    for parameter in &element.parameters {
        let pname: &ParameterName = parameter.name();
        if let Some((axis_index, axis)) = plan
            .axes
            .iter()
            .enumerate()
            .find(|(_, a)| a.target.element == element.name && a.target.parameter == *pname)
        {
            let offset = offsets.get(axis_index).copied().unwrap_or(0) as usize;
            if let Some(v) = axis.values.get(offset) {
                out.insert(pname.clone(), v.clone());
                continue;
            }
        }
        let coord = paramodel_plan::ElementParameterRef::new(
            element.name.clone(),
            pname.clone(),
        );
        if let Some(ConfigEntry::Literal { value }) = plan.bindings.get(&coord) {
            out.insert(pname.clone(), value.clone());
            continue;
        }
        if let Some(ConfigEntry::Literal { value }) = element.configuration.get(pname) {
            out.insert(pname.clone(), value.clone());
            continue;
        }
        if let Some(default) = parameter.default() {
            out.insert(pname.clone(), default);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use paramodel_elements::{
        Element, ElementName, IntegerParameter, LabelValue, Labels, Parameter, ParameterName,
        Value, attributes::label,
    };
    use paramodel_elements::Dependency;
    use paramodel_plan::{
        Axis, AxisName, ElementParameterRef, PlanName, TestPlan, TestPlanId, TestPlanMetadata,
    };

    use super::*;

    fn ename(s: &str) -> ElementName {
        ElementName::new(s).unwrap()
    }
    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }
    fn svc_labels() -> Labels {
        let mut l = Labels::new();
        l.insert(label::r#type(), LabelValue::new("service").unwrap());
        l
    }

    fn plain_service(name: &str) -> Element {
        Element::builder().name(ename(name)).labels(svc_labels()).build()
    }

    fn service_with_axis(name: &str, param: &str) -> Element {
        Element::builder()
            .name(ename(name))
            .labels(svc_labels())
            .parameters(vec![Parameter::Integer(
                IntegerParameter::range(pname(param), 1, 64).unwrap(),
            )])
            .build()
    }

    fn axis_on(element: &str, param: &str, values: Vec<i64>) -> Axis {
        Axis::builder()
            .name(AxisName::new(format!("{element}_{param}_axis")).unwrap())
            .target(ElementParameterRef::new(ename(element), pname(param)))
            .values(
                values
                    .into_iter()
                    .map(|v| Value::integer(pname(param), v, None))
                    .collect(),
            )
            .build()
    }

    /// Transitively depends: does `from` reach `to` through its
    /// `depends_on` chain?
    fn depends_transitively(
        compiled: &ExecutionPlan,
        from:     &str,
        to:       &str,
    ) -> bool {
        let mut stack: Vec<String> = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == from)
            .map(|s| {
                s.depends_on()
                    .iter()
                    .map(|d| d.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        while let Some(curr) = stack.pop() {
            if curr == to {
                return true;
            }
            if !seen.insert(curr.clone()) {
                continue;
            }
            if let Some(s) = compiled.steps().iter().find(|s| s.id().as_str() == curr) {
                for d in s.depends_on() {
                    stack.push(d.as_str().to_owned());
                }
            }
        }
        false
    }

    fn build_plan(elements: Vec<Element>, axes: Vec<Axis>) -> TestPlan {
        TestPlan::builder()
            .id(TestPlanId::from_ulid(Ulid::from_parts(1, 1)))
            .name(PlanName::new("p").unwrap())
            .elements(elements)
            .axes(axes)
            .metadata(
                TestPlanMetadata::builder()
                    .created_at(Timestamp::from_second(1_700_000_000).unwrap())
                    .build(),
            )
            .build()
    }

    // ---------- single-element, single-trial ----------

    #[test]
    fn trivial_plan_compiles_to_deploy_teardown_plus_sentinels() {
        let plan = build_plan(vec![plain_service("db")], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).expect("compiles");
        // start + deploy + teardown + end = 4 steps.
        assert_eq!(compiled.steps().len(), 4);
        // Two checkpoint sentinels.
        assert_eq!(compiled.checkpoints().count(), 2);
    }

    #[test]
    fn trivial_plan_graph_is_linear_chain() {
        let plan = build_plan(vec![plain_service("db")], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();
        let ids: Vec<&str> = compiled.steps().iter().map(|s| s.id().as_str()).collect();
        // deploy depends on start, teardown depends on deploy, end
        // depends on teardown — sort just sanity-checks membership.
        assert!(ids.contains(&"start"));
        assert!(ids.contains(&"end"));
        assert!(ids.contains(&"activate_db_t0"));
        assert!(ids.contains(&"deactivate_db_t0"));
    }

    // ---------- shared dependency ----------

    #[test]
    fn shared_dependency_produces_deploy_edge() {
        let mut client = plain_service("client");
        client.dependencies.push(Dependency::shared(ename("db")));
        let plan = build_plan(vec![plain_service("db"), client], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // Rule 4 inserts notify_trial_start between activate_db_t0 and
        // activate_client_t0, and Rule 8 drops the now-redundant direct
        // edge. The transitive relationship is still intact.
        assert!(
            depends_transitively(&compiled, "activate_client_t0", "activate_db_t0"),
            "activate_client_t0 should still transitively depend on activate_db_t0"
        );
        // Teardown: deactivate_db_t0 transitively depends on
        // deactivate_client_t0 (either directly or via
        // notify_trial_end_t0).
        assert!(
            depends_transitively(&compiled, "deactivate_db_t0", "deactivate_client_t0"),
            "deactivate_db_t0 should transitively depend on deactivate_client_t0"
        );
    }

    // ---------- multi-trial ----------

    #[test]
    fn two_trial_plan_produces_per_trial_deploys() {
        let plan = build_plan(
            vec![service_with_axis("db", "threads")],
            vec![axis_on("db", "threads", vec![1, 2])],
        );
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();
        // db is the only element and it's a trial element (axis-bound),
        // so: start + end + per-trial { deploy, teardown,
        // notify_trial_start, notify_trial_end } × 2 = 10 steps.
        assert_eq!(compiled.steps().len(), 10);
        // Trial-0 deploy has the axis value 1 in its resolved config.
        let deploy_t0 = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_db_t0")
            .expect("deploy t0");
        if let AtomicStep::Deploy { configuration, .. } = deploy_t0 {
            assert_eq!(
                configuration.get(&pname("threads")).and_then(Value::as_integer),
                Some(1)
            );
        } else {
            panic!("wrong variant");
        }
        // Instance graph has 2 instances.
        assert_eq!(compiled.element_instance_graph.total_instances(), 2);
    }

    // ---------- Linear dependency ----------

    #[test]
    fn linear_dependency_produces_deploy_to_teardown_edge() {
        // Use two non-trial elements (neither is a leaf: a third
        // trial element depends on both) so Linear's same-scope check
        // applies and no Rule 4 cycles arise.
        let mut pre = plain_service("pre");
        pre.trial_element = Some(false);
        let mut post = plain_service("post");
        post.trial_element = Some(false);
        post.dependencies.push(Dependency::linear(ename("pre")));
        let mut leaf = plain_service("leaf");
        leaf.dependencies.push(Dependency::shared(ename("post")));
        leaf.trial_element = Some(true);
        let plan = build_plan(vec![pre, post, leaf], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // Linear wired post's activate onto pre's deactivate.
        assert!(
            depends_transitively(&compiled, "activate_post_t0", "deactivate_pre_t0"),
            "Linear edge missing: activate_post_t0 should transitively depend on deactivate_pre_t0"
        );
        // And no direct activate→activate edge.
        let deploy_post = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_post_t0")
            .expect("post deploy");
        assert!(
            !deploy_post
                .depends_on()
                .iter()
                .any(|d| d.as_str() == "activate_pre_t0"),
            "Linear must not add a direct activate→activate edge"
        );
    }

    // ---------- Lifeline dependency ----------

    #[test]
    fn lifeline_dependency_removes_dependent_teardown() {
        let mut client = plain_service("client");
        client.dependencies.push(Dependency::lifeline(ename("db")));
        let plan = build_plan(vec![plain_service("db"), client], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // deactivate_client_t0 does not appear.
        assert!(
            compiled
                .steps()
                .iter()
                .all(|s| s.id().as_str() != "deactivate_client_t0"),
            "Lifeline should remove the dependent's teardown"
        );
        // activate_client_t0 still transitively depends on activate_db_t0
        // (directly, or via notify_trial_start after Rule 4).
        assert!(
            depends_transitively(&compiled, "activate_client_t0", "activate_db_t0"),
            "Lifeline forward edge lost"
        );
    }

    // ---------- Rule 8 — transitive reduction ----------

    #[test]
    fn transitive_reduction_drops_redundant_edges() {
        // a ← b ← c with a → c redundant (implied by a → b → c).
        let mut b = plain_service("b");
        b.dependencies.push(Dependency::shared(ename("a")));
        let mut c = plain_service("c");
        c.dependencies.push(Dependency::shared(ename("a")));
        c.dependencies.push(Dependency::shared(ename("b")));
        let plan = build_plan(vec![plain_service("a"), b, c], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // c is the leaf → trial element; a, b are non-trial. Rule 4
        // inserts notify_trial_start between non-trial activates and
        // the trial activate, so activate_c_t0's direct deps are
        // [notify_trial_start_t0] plus any Rule-8-surviving edges.
        // Assert c is transitively reachable to both a and b.
        assert!(depends_transitively(&compiled, "activate_c_t0", "activate_b_t0"));
        assert!(depends_transitively(&compiled, "activate_c_t0", "activate_a_t0"));

        // The *direct* a → c edge must be removed by Rule 8 (implied
        // by a → b → c).
        let deploy_c = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_c_t0")
            .expect("c deploy");
        assert!(
            !deploy_c
                .depends_on()
                .iter()
                .any(|d| d.as_str() == "activate_a_t0"),
            "Rule 8 should have removed redundant activate_a_t0 edge"
        );
    }

    // ---------- Rule 2 Dedicated ----------

    #[test]
    fn dedicated_dependency_materialises_owner_specific_instance() {
        let mut client = plain_service("client");
        client.dependencies.push(Dependency::dedicated(ename("db")));
        let plan = build_plan(vec![plain_service("db"), client], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // There's a dedicated deploy with the synthetic id.
        let dedicated = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_dedicated_db_for_client_t0")
            .expect("dedicated deploy");
        if let AtomicStep::Deploy {
            dedicated_to,
            instance_number,
            element,
            ..
        } = dedicated
        {
            assert_eq!(dedicated_to.as_ref().map(ElementName::as_str), Some("client"));
            assert_eq!(element.as_str(), "db");
            // Dedicated instances use numbers outside the standalone
            // 0..trial_count range.
            assert!(*instance_number >= 1);
        } else {
            panic!("dedicated step should be Deploy");
        }
        // Client's deploy now depends on the dedicated deploy.
        let client_deploy = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_client_t0")
            .expect("client deploy");
        assert!(
            client_deploy
                .depends_on()
                .iter()
                .any(|d| d.as_str() == "activate_dedicated_db_for_client_t0")
        );
    }

    // ---------- Rule 2 Exclusive ----------

    #[test]
    fn exclusive_dependency_serialises_across_trials() {
        let mut client = plain_service("client");
        client.dependencies.push(Dependency::exclusive(ename("db")));
        // Build db with an axis-bound parameter so two trials are
        // enumerated.
        let mut db = plain_service("db");
        db.parameters.push(Parameter::Integer(
            IntegerParameter::range(pname("threads"), 1, 64).unwrap(),
        ));
        let plan2 = build_plan(
            vec![db, client],
            vec![axis_on("db", "threads", vec![1, 2])],
        );

        let compiled = DefaultCompiler::default().compile(&plan2).unwrap();

        // After Rule 4 rerouting, the direct cross-trial edge is
        // removed; serialisation runs through the notification
        // bridge: deactivate_client_t0 → notify_end_t0 →
        // notify_start_t1 → activate_client_t1.
        assert!(
            depends_transitively(&compiled, "activate_client_t1", "deactivate_client_t0"),
            "Exclusive should still serialise consecutive trials (mediated)"
        );
        // The direct edge must have been removed.
        let client_t1 = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_client_t1")
            .expect("client deploy t1");
        assert!(
            !client_t1
                .depends_on()
                .iter()
                .any(|d| d.as_str() == "deactivate_client_t0"),
            "direct Exclusive cross-trial edge should be rerouted, not direct"
        );
        // And the bridge edge: notify_trial_start_t1 depends on
        // notify_trial_end_t0.
        let start_t1 = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "notify_trial_start_t1")
            .expect("start t1");
        assert!(
            start_t1
                .depends_on()
                .iter()
                .any(|d| d.as_str() == "notify_trial_end_t0"),
            "Exclusive bridge edge missing: notify_start_t1 → notify_end_t0"
        );
    }

    #[test]
    fn cross_prototype_exclusive_on_same_target_emits_w002() {
        let mut a = plain_service("a");
        a.dependencies.push(Dependency::exclusive(ename("db")));
        let mut b = plain_service("b");
        b.dependencies.push(Dependency::exclusive(ename("db")));
        let plan = build_plan(vec![plain_service("db"), a, b], vec![]);
        let err = DefaultCompiler::default().compile(&plan).unwrap_err();
        assert!(
            err.diagnostics
                .iter()
                .any(|d| d.code == WarningCode::W002 && d.message.contains("db")),
            "expected W002, got: {:?}",
            err.diagnostics
        );
    }

    // ---------- Rule 6 ----------

    #[test]
    fn max_group_concurrency_is_stamped_on_deploy() {
        let mut db = plain_service("db");
        db.max_concurrency = Some(4);
        db.max_group_concurrency = Some(2);
        let plan = build_plan(vec![db], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();
        let deploy = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_db_t0")
            .expect("db deploy");
        if let AtomicStep::Deploy {
            max_concurrency,
            max_group_concurrency,
            ..
        } = deploy
        {
            assert_eq!(*max_concurrency, Some(4));
            assert_eq!(*max_group_concurrency, Some(2));
        } else {
            panic!("wrong variant");
        }
    }

    // ---------- Rule 3 — group coalescing ----------

    #[test]
    fn run_scoped_element_coalesces_across_all_trials() {
        // `db` has no axes → effective level 0 → coalesces to a
        // single activate/deactivate across every trial.
        let mut client = service_with_axis("client", "threads");
        client.dependencies.push(Dependency::shared(ename("db")));
        let plan = build_plan(
            vec![plain_service("db"), client],
            vec![axis_on("client", "threads", vec![1, 2, 3])],
        );
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // Exactly one activate and one deactivate for db.
        let db_activates = compiled
            .steps()
            .iter()
            .filter(|s| s.id().as_str().starts_with("activate_db_"))
            .count();
        let db_teardowns = compiled
            .steps()
            .iter()
            .filter(|s| s.id().as_str().starts_with("deactivate_db_"))
            .count();
        assert_eq!(db_activates, 1, "run-scoped db should coalesce to 1 activate");
        assert_eq!(db_teardowns, 1, "run-scoped db should coalesce to 1 deactivate");

        // The kept ids are the first-trial activate and the last-trial
        // deactivate — `activate_db_t0` and `deactivate_db_t2`.
        assert!(
            compiled
                .steps()
                .iter()
                .any(|s| s.id().as_str() == "activate_db_t0")
        );
        assert!(
            compiled
                .steps()
                .iter()
                .any(|s| s.id().as_str() == "deactivate_db_t2")
        );

        // Client activates still per-trial (client is a trial element
        // by identification).
        let client_activates = compiled
            .steps()
            .iter()
            .filter(|s| s.id().as_str().starts_with("activate_client_"))
            .count();
        assert_eq!(client_activates, 3);
    }

    #[test]
    fn trial_element_is_not_coalesced() {
        // When only trial elements exist, nothing to coalesce.
        let client = service_with_axis("client", "threads");
        let plan = build_plan(
            vec![client],
            vec![axis_on("client", "threads", vec![1, 2])],
        );
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();
        let activates = compiled
            .steps()
            .iter()
            .filter(|s| s.id().as_str().starts_with("activate_client_"))
            .count();
        assert_eq!(activates, 2);
    }

    #[test]
    fn coalesced_shared_edges_rewire_onto_kept_node() {
        let mut client = service_with_axis("client", "threads");
        client.dependencies.push(Dependency::shared(ename("db")));
        let plan = build_plan(
            vec![plain_service("db"), client],
            vec![axis_on("client", "threads", vec![1, 2])],
        );
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // db coalesced to a single activate (t0) and deactivate (t1).
        // activate_db_t1 and deactivate_db_t0 should be gone.
        assert!(
            compiled
                .steps()
                .iter()
                .any(|s| s.id().as_str() == "activate_db_t0")
        );
        assert!(
            compiled
                .steps()
                .iter()
                .all(|s| s.id().as_str() != "activate_db_t1"),
            "coalesced activate_db_t1 should be removed"
        );
        assert!(
            compiled
                .steps()
                .iter()
                .any(|s| s.id().as_str() == "deactivate_db_t1")
        );
        assert!(
            compiled
                .steps()
                .iter()
                .all(|s| s.id().as_str() != "deactivate_db_t0"),
            "coalesced deactivate_db_t0 should be removed"
        );

        // Both client trials still transitively reach the coalesced
        // activate.
        assert!(depends_transitively(
            &compiled,
            "activate_client_t0",
            "activate_db_t0",
        ));
        assert!(depends_transitively(
            &compiled,
            "activate_client_t1",
            "activate_db_t0",
        ));
        // Coalesced teardown waits (transitively) on both client
        // teardowns.
        assert!(depends_transitively(
            &compiled,
            "deactivate_db_t1",
            "deactivate_client_t0",
        ));
        assert!(depends_transitively(
            &compiled,
            "deactivate_db_t1",
            "deactivate_client_t1",
        ));
    }

    // ---------- Rule 4 — trial notifications ----------

    #[test]
    fn rule4_inserts_notify_trial_start_and_end_per_trial() {
        let mut client = service_with_axis("client", "threads");
        client.dependencies.push(Dependency::shared(ename("db")));
        let plan = build_plan(
            vec![plain_service("db"), client],
            vec![axis_on("client", "threads", vec![1, 2])],
        );
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // One pair per trial.
        let starts = compiled
            .steps()
            .iter()
            .filter(|s| s.id().as_str().starts_with("notify_trial_start_"))
            .count();
        let ends = compiled
            .steps()
            .iter()
            .filter(|s| s.id().as_str().starts_with("notify_trial_end_"))
            .count();
        assert_eq!(starts, 2);
        assert_eq!(ends, 2);

        // notify_trial_start_t0 depends on activate_db_t0 (the
        // coalesced non-trial activate).
        let ts0 = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "notify_trial_start_t0")
            .expect("ts0");
        assert!(
            ts0.depends_on()
                .iter()
                .any(|d| d.as_str() == "activate_db_t0"),
            "notify_trial_start_t0 must gate on activate_db_t0"
        );
        // activate_client_t0 depends on notify_trial_start_t0.
        let client_t0 = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_client_t0")
            .expect("client t0");
        assert!(
            client_t0
                .depends_on()
                .iter()
                .any(|d| d.as_str() == "notify_trial_start_t0")
        );
    }

    #[test]
    fn rule4_skipped_when_no_trial_elements() {
        // A single plain service with no axes and nothing depending
        // on it is floating → not a trial element.
        let plan = build_plan(vec![plain_service("solo")], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();
        assert!(
            compiled
                .steps()
                .iter()
                .all(|s| !s.id().as_str().starts_with("notify_trial_")),
            "no trial elements → no notifications"
        );
    }

    // ---------- Rule 5 — health-check gates ----------

    #[test]
    fn rule5_inserts_readiness_gate_for_health_checked_element() {
        use paramodel_elements::HealthCheckSpec;
        use std::time::Duration;

        let mut db = plain_service("db");
        db.health_check = Some(HealthCheckSpec::new(
            Duration::from_secs(5),
            3,
            Duration::from_millis(500),
        ));
        let mut client = plain_service("client");
        client.dependencies.push(Dependency::shared(ename("db")));
        let plan = build_plan(vec![db, client], vec![]);
        let compiled = DefaultCompiler::default().compile(&plan).unwrap();

        // A Barrier with kind ElementReady exists for db's activate.
        let gate = compiled.steps().iter().find(|s| {
            matches!(
                s,
                AtomicStep::Barrier {
                    barrier_kind: BarrierKind::ElementReady,
                    ..
                }
            )
        });
        assert!(gate.is_some(), "expected an ElementReady barrier for gated db");

        // activate_client_t0 no longer directly depends on
        // activate_db_t0 — it depends on the gate instead.
        let client_deploy = compiled
            .steps()
            .iter()
            .find(|s| s.id().as_str() == "activate_client_t0")
            .expect("client deploy");
        assert!(
            !client_deploy
                .depends_on()
                .iter()
                .any(|d| d.as_str() == "activate_db_t0"),
            "dependents must route through the gate, not the raw activate"
        );
        // Still transitively reaches db's activate.
        assert!(depends_transitively(
            &compiled,
            "activate_client_t0",
            "activate_db_t0",
        ));
    }

    // ---------- unsupported features ----------

    #[test]
    fn command_element_is_rejected() {
        let mut cmd = plain_service("cmd");
        cmd.shutdown_semantics = ShutdownSemantics::Command;
        let plan = build_plan(vec![cmd], vec![]);
        let err = DefaultCompiler::default().compile(&plan).unwrap_err();
        assert!(err
            .diagnostics
            .iter()
            .any(|d| d.code == WarningCode::E002 && d.message.contains("Command")));
    }

    #[test]
    fn token_configuration_is_rejected() {
        let mut element = service_with_axis("db", "threads");
        element.configuration.insert(
            pname("threads"),
            ConfigEntry::token(paramodel_elements::TokenExpr::new("${self.ip}").unwrap()),
        );
        let plan = build_plan(vec![element], vec![]);
        let err = DefaultCompiler::default().compile(&plan).unwrap_err();
        assert!(err
            .diagnostics
            .iter()
            .any(|d| d.code == WarningCode::E002 && d.message.contains("token")));
    }

    // ---------- validate() ----------

    #[test]
    fn validate_reports_unsupported_without_compiling() {
        let mut db = plain_service("db");
        db.shutdown_semantics = ShutdownSemantics::Command;
        let plan = build_plan(vec![db], vec![]);
        let diags = DefaultCompiler::default().validate(&plan);
        assert!(diags.iter().any(|d| d.code == WarningCode::E002));
    }
}
