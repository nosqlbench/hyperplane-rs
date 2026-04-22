// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `PersistenceError` + `PersistenceResult`.
//!
//! Shared across all six stores so callers can match once rather
//! than per-trait. Per SRD-0012 D14: specific variants for
//! `NotFound`, `Conflict`, `VersionMismatch`, `Serialization`,
//! `Backend`, `Retention`, `InvalidQuery`; backend-specific detail
//! travels as a boxed source error on `Serialization` / `Backend`.

use thiserror::Error;

/// Errors produced by any persistence trait.
#[derive(Debug, Error)]
pub enum PersistenceError {
    /// The requested record does not exist.
    #[error("not found: {kind} / {id}")]
    NotFound {
        /// Record kind (`"execution_plan"`, `"trial_result"`, …).
        kind: &'static str,
        /// Identifier the caller supplied.
        id:   String,
    },

    /// A different payload already exists under this key.
    #[error("conflict: {kind} / {id} already exists")]
    Conflict {
        /// Record kind.
        kind: &'static str,
        /// Identifier.
        id:   String,
    },

    /// Optimistic-concurrency check failed.
    #[error("version mismatch on {kind} / {id}: expected {expected}, found {actual}")]
    VersionMismatch {
        /// Record kind.
        kind:     &'static str,
        /// Identifier.
        id:       String,
        /// Expected version.
        expected: String,
        /// Actual version the backend reports.
        actual:   String,
    },

    /// Serialisation or deserialisation failed.
    #[error("serialisation failure: {0}")]
    Serialization(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Backend-specific failure.
    #[error("backend error: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// Retention policy rejected the requested operation.
    #[error("retention policy violation: {0}")]
    Retention(String),

    /// Filter or aggregation rejected by the backend.
    #[error("invalid query: {0}")]
    InvalidQuery(String),
}

/// Result alias used throughout `paramodel-persistence`.
pub type PersistenceResult<T> = std::result::Result<T, PersistenceError>;

impl PersistenceError {
    /// Shorthand for [`Self::NotFound`].
    #[must_use]
    pub fn not_found(kind: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound {
            kind,
            id: id.into(),
        }
    }

    /// Shorthand for [`Self::Conflict`].
    #[must_use]
    pub fn conflict(kind: &'static str, id: impl Into<String>) -> Self {
        Self::Conflict {
            kind,
            id: id.into(),
        }
    }

    /// Shorthand for [`Self::Backend`].
    #[must_use]
    pub fn backend<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Backend(Box::new(err))
    }

    /// Shorthand for [`Self::Serialization`].
    #[must_use]
    pub fn serialization<E>(err: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Self::Serialization(Box::new(err))
    }
}
