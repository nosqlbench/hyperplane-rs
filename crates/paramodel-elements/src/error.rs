// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Crate-local error type for `paramodel-elements`.
//!
//! Per SRD-0003 D3, each paramodel crate owns a single `Error` enum
//! and a local `Result<T>` alias. `paramodel-elements` is now the
//! central algebra crate (after absorbing the former `paramodel-core`),
//! so everything below ships here: name validation, domain / parameter
//! / attribute / element / trial construction, regex compilation, and
//! derivation evaluation.

use crate::attributes::AttributeError;
use crate::domain::DomainError;
use crate::expression::DerivationError;
use crate::names::NameError;
use crate::parameter::ParameterError;
use crate::trial::TrialError;

/// Errors produced by `paramodel-elements`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A validated name (parameter, element, port, …) failed its
    /// construction check.
    #[error(transparent)]
    Name(#[from] NameError),

    /// A domain constructor or operation was rejected.
    #[error(transparent)]
    Domain(#[from] DomainError),

    /// An attribute layer constructor or validation was rejected.
    #[error(transparent)]
    Attribute(#[from] AttributeError),

    /// A parameter constructor or builder setter was rejected.
    #[error(transparent)]
    Parameter(#[from] ParameterError),

    /// A derived-parameter expression failed at bind time.
    #[error(transparent)]
    Derivation(#[from] DerivationError),

    /// A regex pattern failed to compile.
    #[error("invalid regex: {0}")]
    Regex(#[from] regex::Error),

    /// An element-level validation or construction was rejected.
    #[error(transparent)]
    Element(#[from] ElementError),

    /// A trial constructor (assignments, sampling strategy, ...)
    /// rejected its inputs.
    #[error(transparent)]
    Trial(#[from] TrialError),
}

/// Crate-local result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

// ---------------------------------------------------------------------------
// ElementError.
// ---------------------------------------------------------------------------

/// Errors from `Element` construction and validation.
#[derive(Debug, thiserror::Error)]
pub enum ElementError {
    /// A name-validation error bubbled up (e.g. the `type` label value
    /// wasn't a valid `TypeId`).
    #[error(transparent)]
    Name(#[from] NameError),

    /// An attribute-layer namespace check failed.
    #[error(transparent)]
    Attribute(#[from] AttributeError),

    /// The element's `labels` are missing the `type` entry required
    /// by SRD-0007 D8.
    #[error("element is missing the required `type` label")]
    MissingTypeLabel,

    /// The element's `type` label names a type that isn't registered.
    #[error("unknown element type '{type_id}'")]
    UnknownElementType {
        /// The unregistered type id.
        type_id: String,
    },

    /// Two `Parameter`s in `Element.parameters` share a name.
    #[error("duplicate parameter name '{name}' in element parameters")]
    DuplicateParameterName {
        /// The duplicated name.
        name: String,
    },

    /// Two `Parameter`s in `Element.result_parameters` share a name.
    #[error("duplicate parameter name '{name}' in element result_parameters")]
    DuplicateResultParameterName {
        /// The duplicated name.
        name: String,
    },

    /// A configuration key references a parameter the element doesn't
    /// declare.
    #[error("configuration entry references unknown parameter '{name}'")]
    UnknownConfigurationParameter {
        /// The offending parameter name.
        name: String,
    },

    /// A parameter name collides with a label / tag / port on the
    /// same element.
    #[error("parameter name '{name}' collides with an attribute or port name")]
    ParameterNameCollidesWithAttribute {
        /// The offending parameter name.
        name: String,
    },

    /// `max_concurrency` is set to zero.
    #[error("max_concurrency must be >= 1")]
    InvalidMaxConcurrency,

    /// `max_group_concurrency` exceeds `max_concurrency`.
    #[error("max_group_concurrency ({group}) exceeds max_concurrency ({global})")]
    GroupConcurrencyExceedsGlobal {
        /// The group-level cap.
        group:  u32,
        /// The global cap it must not exceed.
        global: u32,
    },

    /// A required label (declared by the type descriptor) is absent.
    #[error("element is missing required label '{key}' for its type")]
    MissingRequiredLabel {
        /// The label key.
        key: String,
    },

    /// A forbidden label (declared by the type descriptor) is present.
    #[error("forbidden label '{key}' is present on this element: {reason}")]
    ForbiddenLabelPresent {
        /// The offending label key.
        key:    String,
        /// Descriptor-supplied explanation.
        reason: String,
    },

    /// A `TokenExpr` was constructed from an empty string.
    #[error("token expression must not be empty")]
    EmptyTokenExpr,
}
