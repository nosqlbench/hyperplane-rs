// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Machine-generated ULID identifiers.
//!
//! Per SRD-0003 D8, paramodel uses ULIDs for identities that need to be
//! unique, lexicographically sortable by creation time, and portable
//! across processes. [`TrialId`] is the canonical trial identifier; it
//! shows up in `TrialContext`, `Sequence` coordinates, and anywhere the
//! system needs to refer to a single trial run.
//!
//! Constructors take a pre-generated `Ulid` rather than calling
//! `Ulid::new()` internally — generation policy (seeded or
//! thread-random) belongs at the plan-executor layer so paramodel-core
//! stays free of hidden mutable state (SRD-0003 R9).

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Unique identifier of one trial run.
///
/// Lexicographic ordering on the underlying ULID orders trials by
/// creation instant, so a sorted list of `TrialId`s is also sorted by
/// time-of-creation within a single generator stream.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct TrialId(Ulid);

impl TrialId {
    /// Construct from an existing ULID.
    #[must_use]
    pub const fn from_ulid(u: Ulid) -> Self {
        Self(u)
    }

    /// Borrow the inner ULID.
    #[must_use]
    pub const fn as_ulid(&self) -> &Ulid {
        &self.0
    }

    /// Consume and return the inner ULID.
    #[must_use]
    pub const fn into_ulid(self) -> Ulid {
        self.0
    }
}

impl std::fmt::Display for TrialId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl From<Ulid> for TrialId {
    fn from(u: Ulid) -> Self {
        Self(u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trial_id_roundtrips_ulid() {
        let u = Ulid::from_parts(1_700_000_000_000, 42);
        let t = TrialId::from_ulid(u);
        assert_eq!(t.as_ulid(), &u);
        assert_eq!(t.into_ulid(), u);
    }

    #[test]
    fn trial_id_display_matches_ulid_display() {
        let u = Ulid::from_parts(1_700_000_000_000, 42);
        let t = TrialId::from_ulid(u);
        assert_eq!(format!("{t}"), format!("{u}"));
    }

    #[test]
    fn trial_id_is_lexicographically_ordered_by_time() {
        let earlier = TrialId::from_ulid(Ulid::from_parts(1_000, 0));
        let later = TrialId::from_ulid(Ulid::from_parts(2_000, 0));
        assert!(earlier < later);
    }

    #[test]
    fn trial_id_serde_roundtrip() {
        let t = TrialId::from_ulid(Ulid::from_parts(1_700_000_000_000, 42));
        let json = serde_json::to_string(&t).unwrap();
        let back: TrialId = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }
}
