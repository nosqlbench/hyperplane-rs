// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Crate-local error type for the test-plan layer.

use paramodel_elements::NameError;

/// Errors from `Axis` and `TestPlan` construction / validation.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// A name newtype failed validation.
    #[error(transparent)]
    Name(#[from] NameError),

    /// Plan-binding keys or axis targets pointed at a parameter kind
    /// that doesn't match the axis values — paramodel-elements
    /// errors bubble through this variant for cases where element
    /// construction is involved.
    #[error(transparent)]
    Elements(#[from] paramodel_elements::Error),

    /// Two elements in a plan share a name.
    #[error("duplicate element name '{name}' in plan.elements")]
    DuplicateElement {
        /// The duplicated name.
        name: String,
    },

    /// Axis targets an element that isn't in the plan.
    #[error("axis '{axis}' targets element '{element}', which is not in plan.elements")]
    AxisTargetElementNotFound {
        /// Axis name.
        axis:    String,
        /// Missing element name.
        element: String,
    },

    /// Axis targets a parameter the owning element doesn't declare.
    #[error(
        "axis '{axis}' targets parameter '{parameter}' on element '{element}', \
         which is not in the element's parameters"
    )]
    AxisTargetParameterNotFound {
        /// Axis name.
        axis:      String,
        /// Element name.
        element:   String,
        /// Missing parameter name.
        parameter: String,
    },

    /// Axis value kind doesn't match the target parameter's kind.
    #[error(
        "axis '{axis}' value at index {index} has kind {actual:?}, but the target \
         parameter is of kind {expected:?}"
    )]
    AxisValueKindMismatch {
        /// Axis name.
        axis:     String,
        /// Index of the first mismatch.
        index:    usize,
        /// The kind the target parameter declared.
        expected: paramodel_elements::ValueKind,
        /// The kind the value actually carries.
        actual:   paramodel_elements::ValueKind,
    },

    /// Axis targets a derived parameter (SRD-0004 D9).
    #[error(
        "axis '{axis}' targets derived parameter '{parameter}' on element '{element}'; \
         derived parameters are not eligible as axes"
    )]
    DerivedParameterCannotBeAxis {
        /// Axis name.
        axis:      String,
        /// Element name.
        element:   String,
        /// Parameter name.
        parameter: String,
    },

    /// Two axes share a name.
    #[error("duplicate axis name '{name}' in plan")]
    DuplicateAxisName {
        /// The duplicated name.
        name: String,
    },

    /// Two axes target the same `(element, parameter)` coordinate.
    #[error(
        "axes '{axis_a}' and '{axis_b}' both target ({element}, {parameter}); \
         only one axis may vary a given parameter"
    )]
    DuplicateAxisTarget {
        /// First axis.
        axis_a:    String,
        /// Second axis.
        axis_b:    String,
        /// Shared element name.
        element:   String,
        /// Shared parameter name.
        parameter: String,
    },

    /// Plan-binding targets an element that isn't in the plan.
    #[error("plan binding targets element '{element}', which is not in plan.elements")]
    BindingTargetElementNotFound {
        /// Missing element name.
        element: String,
    },

    /// Plan-binding targets a parameter the owning element doesn't
    /// declare.
    #[error(
        "plan binding targets parameter '{parameter}' on element '{element}', \
         which is not in the element's parameters"
    )]
    BindingTargetParameterNotFound {
        /// Element name.
        element:   String,
        /// Missing parameter name.
        parameter: String,
    },

    /// A plan-binding key coincides with an axis's target (would be
    /// overridden; reject as authoring error per D12).
    #[error(
        "plan binding for ({element}, {parameter}) overlaps axis '{axis}'; \
         a coordinate may be bound by exactly one source"
    )]
    BindingOverlapsAxis {
        /// Element name.
        element:   String,
        /// Parameter name.
        parameter: String,
        /// Axis name that claimed the coordinate first.
        axis:      String,
    },

    /// An axis carries zero values.
    #[error("axis '{axis}' has no values")]
    AxisValuesEmpty {
        /// Axis name.
        axis: String,
    },

    /// An axis carries duplicate values.
    #[error("axis '{axis}' has duplicate values")]
    AxisDuplicateValue {
        /// Axis name.
        axis: String,
    },

    /// An axis's `boundary_values` is not a subset of `values`.
    #[error(
        "axis '{axis}' boundary_values is not a subset of values; every boundary \
         must appear in the axis's value list"
    )]
    AxisBoundaryNotSubset {
        /// Axis name.
        axis: String,
    },

    /// `RetryPolicy.max_attempts == 0`.
    #[error("retry policy max_attempts must be >= 1")]
    ZeroMaxAttempts,

    /// `with_axes_reordered` received an order that isn't a permutation
    /// of the plan's existing axes.
    #[error("invalid axis order: {reason}")]
    InvalidAxisOrder {
        /// Why it's invalid.
        reason: String,
    },

    /// A `StepId` / `BarrierId` / `CheckpointId` candidate failed
    /// validation.
    #[error("invalid step identifier: {reason}")]
    InvalidStepId {
        /// Why it's invalid.
        reason: String,
    },

    /// Two steps share an id within an `ExecutionGraph`.
    #[error("duplicate step id '{id}' in execution graph")]
    DuplicateStepId {
        /// The duplicated id.
        id: String,
    },

    /// A step's `depends_on` references an id that isn't in the graph.
    #[error("step '{step}' depends on '{missing}', which is not in the graph")]
    DanglingDependency {
        /// The step with the dangling reference.
        step:    String,
        /// The id that isn't present.
        missing: String,
    },

    /// The execution graph has a cycle.
    #[error("execution graph is not acyclic")]
    GraphHasCycle,

    /// Two `ElementInstance`s share an id within one
    /// `ElementInstanceGraph`.
    #[error("duplicate element-instance id '{id}' in element instance graph")]
    DuplicateInstance {
        /// The duplicated id.
        id: String,
    },

    /// An `InstanceDependency` edge references an instance that isn't
    /// in the graph.
    #[error("instance edge references unknown instance '{id}'")]
    UnknownInstance {
        /// The missing instance id.
        id: String,
    },
}

/// Crate-local result alias.
pub type Result<T, E = PlanError> = std::result::Result<T, E>;
