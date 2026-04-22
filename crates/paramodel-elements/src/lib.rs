// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Paramodel — the central algebra crate.
//!
//! Everything paramodel models lives here: the parameter algebra
//! (parameter / domain / constraint / value / validation / expression),
//! the attribute layer (labels, tags, plugs, sockets, wires), the
//! element model (`Element`, `Dependency`, runtime), and the
//! trial-as-element-parameterization layer (`Trial`, `Assignments`).
//!
//! The crate was formerly split into `paramodel-core` and
//! `paramodel-elements`, with `Trial` living in `paramodel-trials`.
//! That split cost indirection without buying value — `Trial` is a
//! specific parameterization of an element graph over specific
//! coordinates, which puts it in the same semantic unit as `Element`.
//! Downstream specialisation (trial sets, outcomes, plans,
//! compilation, execution) still lives in sibling crates — this one
//! is the algebra they're built on.

// ---- parameter algebra + core primitives (formerly paramodel-core) ----
pub mod attributes;
pub mod constraint;
pub mod domain;
pub mod error;
pub mod expression;
pub mod fingerprint;
pub mod ids;
pub mod names;
pub mod parameter;
pub mod validation;
pub mod value;

// ---- trial as a parameterization of the element graph ----
pub mod trial;

// ---- element model (declarative + runtime) ----
pub mod configuration;
pub mod dependency;
pub mod element;
pub mod lifecycle;
pub mod runtime;
pub mod types;

// ---------------------------------------------------------------------------
// Public re-exports.
// ---------------------------------------------------------------------------

pub use attributes::{
    AttributeError, Attributed, Facet, FacetKey, FacetValue, LabelKey, LabelValue, Labels, Plug,
    Pluggable, PortName, Socket, TagKey, TagValue, Tags, Tier, Wire, WireMatch, fits,
    validate_namespace, wiring_for,
};
pub use configuration::{ConfigEntry, Configuration, ExportName, Exports, TokenExpr};
pub use constraint::{
    BoolConstraint, Constraint, DoubleConstraint, IntConstraint, SelectionConstraint,
    StringConstraint,
};
pub use dependency::{Dependency, RelationshipType};
pub use domain::{
    Cardinality, Domain, DomainError, DoubleDomain, IntegerDomain, LabeledEntry,
    LabeledSelectionResolver, RegexPattern, ResolverId, SelectionDomain, SelectionResolver,
    SelectionResolverRegistry, StringDomain,
};
pub use element::Element;
pub use error::{ElementError, Error, Result};
pub use expression::{
    BinOp, BuiltinFn, DerivationError, EvalValue, Expression, Literal, UnOp, ValueBindings,
};
pub use fingerprint::{Fingerprint, FingerprintBuilder};
pub use ids::TrialId;
pub use lifecycle::{
    HealthCheckSpec, LiveStatusSummary, OperationalState, ShutdownSemantics, StateTransition,
};
pub use names::{ElementName, Name, NameError, ParameterName};
pub use parameter::{
    BooleanParameter, DerivedParameter, DoubleParameter, IntegerParameter, Parameter,
    ParameterError, SelectionParameter, StringParameter,
};
pub use runtime::{
    ElementRuntime, ElementRuntimeRegistry, MaterializationOutputs, ResolvedConfiguration,
    StateObservation, StateTransitionListener, TrialContext,
};
pub use trial::{Assignments, TRIAL_TAG, Trial, TrialError, TrialMetadata};
pub use types::{
    ElementTypeDescriptor, ElementTypeDescriptorRegistry, OpenRegistry, TypeId,
};
pub use validation::ValidationResult;
pub use value::{
    BooleanValue, BoundaryKind, DoubleValue, GeneratorInfo, IntegerValue, Provenance,
    SelectionItem, SelectionValue, StringValue, Value, ValueKind,
};
