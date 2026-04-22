// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Executor-layer identifiers (`ExecutionId`, `AllocationId`,
//! `JournalSequence`).

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Unique identifier for one run of an `ExecutionPlan`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct ExecutionId(Ulid);

impl ExecutionId {
    /// From a pre-generated ULID.
    #[must_use]
    pub const fn from_ulid(u: Ulid) -> Self {
        Self(u)
    }

    /// Borrow the inner ULID.
    #[must_use]
    pub const fn as_ulid(&self) -> &Ulid {
        &self.0
    }
}

impl std::fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Unique identifier for one resource allocation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct AllocationId(Ulid);

impl AllocationId {
    /// From a pre-generated ULID.
    #[must_use]
    pub const fn from_ulid(u: Ulid) -> Self {
        Self(u)
    }
}

impl std::fmt::Display for AllocationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Monotonic journal sequence within a single `ExecutionId`.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
)]
#[serde(transparent)]
pub struct JournalSequence(u64);

impl JournalSequence {
    /// Zero, the starting sequence.
    pub const ZERO: Self = Self(0);

    /// From a raw `u64`.
    #[must_use]
    pub const fn new(v: u64) -> Self {
        Self(v)
    }

    /// Raw inner value.
    #[must_use]
    pub const fn get(&self) -> u64 {
        self.0
    }

    /// Next sequence. Saturates at `u64::MAX`.
    #[must_use]
    pub const fn next(&self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for JournalSequence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_sequence_next_monotonic() {
        let a = JournalSequence::ZERO;
        let b = a.next();
        let c = b.next();
        assert!(a < b);
        assert!(b < c);
        assert_eq!(c.get(), 2);
    }

    #[test]
    fn ids_serde_roundtrip() {
        let e = ExecutionId::from_ulid(Ulid::from_parts(1, 1));
        let json = serde_json::to_string(&e).unwrap();
        let back: ExecutionId = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
