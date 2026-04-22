// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Hand-rolled `Serialize` / `Deserialize` for
//! [`ResultFilter`][super::result_store::ResultFilter],
//! [`Aggregation`][super::result_store::Aggregation], and
//! [`AggregateResult`][super::result_store::AggregateResult].
//!
//! Why hand-rolled? `#[derive(Serialize, Deserialize)]` on these
//! recursive enums combined with the wide [`Value`] enum in leaves
//! drives rustc's trait-resolution recursion past any practical
//! limit (reproducible on nightly + serde 1.0.228). The derive-
//! macro expansion produces quadratic bounds that never converge.
//!
//! Format matches what serde-derive would emit for
//! `#[serde(tag = "kind", rename_all = "snake_case")]` — a JSON-
//! style object with a `"kind"` discriminator and variant fields
//! alongside. `"kind"` must appear first in the encoded stream; all
//! derived serialisers obey that, and the deserialiser enforces it.

use std::fmt;

use paramodel_elements::{LabelKey, LabelValue, TagKey, TagValue, TrialId, Value};
use paramodel_executor::ExecutionId;
use paramodel_plan::ElementParameterRef;
use paramodel_trials::TrialStatus;
use serde::de::{Error as DeError, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::result_store::{
    AggregateResult, Aggregation, Comparison, GroupDimension, ResultFilter,
    TrialCodePattern,
};

// ---------------------------------------------------------------------------
// ResultFilter.
// ---------------------------------------------------------------------------

impl Serialize for ResultFilter {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut m = ser.serialize_map(None)?;
        match self {
            Self::Any => {
                m.serialize_entry("kind", "any")?;
            }
            Self::TrialId { id } => {
                m.serialize_entry("kind", "trial_id")?;
                m.serialize_entry("id", id)?;
            }
            Self::ExecutionId { id } => {
                m.serialize_entry("kind", "execution_id")?;
                m.serialize_entry("id", id)?;
            }
            Self::PlanFingerprint { fp } => {
                m.serialize_entry("kind", "plan_fingerprint")?;
                m.serialize_entry("fp", fp)?;
            }
            Self::Status { status } => {
                m.serialize_entry("kind", "status")?;
                m.serialize_entry("status", status)?;
            }
            Self::StatusIn { statuses } => {
                m.serialize_entry("kind", "status_in")?;
                m.serialize_entry("statuses", statuses)?;
            }
            Self::StartedAfter { ts } => {
                m.serialize_entry("kind", "started_after")?;
                m.serialize_entry("ts", ts)?;
            }
            Self::StartedBefore { ts } => {
                m.serialize_entry("kind", "started_before")?;
                m.serialize_entry("ts", ts)?;
            }
            Self::AttemptNumber { cmp, value } => {
                m.serialize_entry("kind", "attempt_number")?;
                m.serialize_entry("cmp", cmp)?;
                m.serialize_entry("value", value)?;
            }
            Self::Metric { coord, cmp, value } => {
                m.serialize_entry("kind", "metric")?;
                m.serialize_entry("coord", coord)?;
                m.serialize_entry("cmp", cmp)?;
                m.serialize_entry("value", value)?;
            }
            Self::Assignment { coord, value } => {
                m.serialize_entry("kind", "assignment")?;
                m.serialize_entry("coord", coord)?;
                m.serialize_entry("value", value)?;
            }
            Self::TrialCode { pattern } => {
                m.serialize_entry("kind", "trial_code")?;
                m.serialize_entry("pattern", pattern)?;
            }
            Self::LabelEquals { key, value } => {
                m.serialize_entry("kind", "label_equals")?;
                m.serialize_entry("key", key)?;
                m.serialize_entry("value", value)?;
            }
            Self::TagEquals { key, value } => {
                m.serialize_entry("kind", "tag_equals")?;
                m.serialize_entry("key", key)?;
                m.serialize_entry("value", value)?;
            }
            Self::And { children } => {
                m.serialize_entry("kind", "and")?;
                m.serialize_entry("children", children)?;
            }
            Self::Or { children } => {
                m.serialize_entry("kind", "or")?;
                m.serialize_entry("children", children)?;
            }
            Self::Not { child } => {
                m.serialize_entry("kind", "not")?;
                m.serialize_entry("child", child)?;
            }
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for ResultFilter {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_map(ResultFilterVisitor)
    }
}

struct ResultFilterVisitor;

impl<'de> Visitor<'de> for ResultFilterVisitor {
    type Value = ResultFilter;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a ResultFilter map with a `kind` tag")
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> Result<Self::Value, A::Error> {
        let kind = read_kind(&mut map)?;
        let out = match kind.as_str() {
            "any" => ResultFilter::Any,
            "trial_id" => ResultFilter::TrialId {
                id: read_named(&mut map, "id")?,
            },
            "execution_id" => ResultFilter::ExecutionId {
                id: read_named::<_, ExecutionId>(&mut map, "id")?,
            },
            "plan_fingerprint" => ResultFilter::PlanFingerprint {
                fp: read_named(&mut map, "fp")?,
            },
            "status" => ResultFilter::Status {
                status: read_named::<_, TrialStatus>(&mut map, "status")?,
            },
            "status_in" => ResultFilter::StatusIn {
                statuses: read_named(&mut map, "statuses")?,
            },
            "started_after" => ResultFilter::StartedAfter {
                ts: read_named(&mut map, "ts")?,
            },
            "started_before" => ResultFilter::StartedBefore {
                ts: read_named(&mut map, "ts")?,
            },
            "attempt_number" => {
                let cmp = read_named::<_, Comparison>(&mut map, "cmp")?;
                let value = read_named::<_, u32>(&mut map, "value")?;
                ResultFilter::AttemptNumber { cmp, value }
            }
            "metric" => {
                let coord = read_named::<_, ElementParameterRef>(&mut map, "coord")?;
                let cmp = read_named::<_, Comparison>(&mut map, "cmp")?;
                let value = read_named::<_, Value>(&mut map, "value")?;
                ResultFilter::Metric { coord, cmp, value }
            }
            "assignment" => {
                let coord = read_named::<_, ElementParameterRef>(&mut map, "coord")?;
                let value = read_named::<_, Value>(&mut map, "value")?;
                ResultFilter::Assignment { coord, value }
            }
            "trial_code" => ResultFilter::TrialCode {
                pattern: read_named::<_, TrialCodePattern>(&mut map, "pattern")?,
            },
            "label_equals" => {
                let key = read_named::<_, LabelKey>(&mut map, "key")?;
                let value = read_named::<_, LabelValue>(&mut map, "value")?;
                ResultFilter::LabelEquals { key, value }
            }
            "tag_equals" => {
                let key = read_named::<_, TagKey>(&mut map, "key")?;
                let value = read_named::<_, TagValue>(&mut map, "value")?;
                ResultFilter::TagEquals { key, value }
            }
            "and" => ResultFilter::And {
                children: read_named(&mut map, "children")?,
            },
            "or" => ResultFilter::Or {
                children: read_named(&mut map, "children")?,
            },
            "not" => ResultFilter::Not {
                child: read_named(&mut map, "child")?,
            },
            other => {
                return Err(A::Error::custom(format!(
                    "unknown ResultFilter kind `{other}`"
                )));
            }
        };
        drain_rest(map)?;
        Ok(out)
    }

    // Silence unused-trial-id-import warning when we never need it.
    fn visit_unit<E: DeError>(self) -> Result<Self::Value, E> {
        Err(E::custom("ResultFilter cannot deserialize from unit"))
    }
}

// Silence unused-lint on TrialId in this module by referencing it.
#[allow(dead_code)]
const _: fn(&TrialId) = |_| {};

// ---------------------------------------------------------------------------
// Aggregation.
// ---------------------------------------------------------------------------

impl Serialize for Aggregation {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut m = ser.serialize_map(None)?;
        match self {
            Self::Count => {
                m.serialize_entry("kind", "count")?;
            }
            Self::Min { metric } => {
                m.serialize_entry("kind", "min")?;
                m.serialize_entry("metric", metric)?;
            }
            Self::Max { metric } => {
                m.serialize_entry("kind", "max")?;
                m.serialize_entry("metric", metric)?;
            }
            Self::Sum { metric } => {
                m.serialize_entry("kind", "sum")?;
                m.serialize_entry("metric", metric)?;
            }
            Self::Avg { metric } => {
                m.serialize_entry("kind", "avg")?;
                m.serialize_entry("metric", metric)?;
            }
            Self::Percentile { metric, p } => {
                m.serialize_entry("kind", "percentile")?;
                m.serialize_entry("metric", metric)?;
                m.serialize_entry("p", p)?;
            }
            Self::GroupBy { dimension, then } => {
                m.serialize_entry("kind", "group_by")?;
                m.serialize_entry("dimension", dimension)?;
                m.serialize_entry("then", then)?;
            }
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Aggregation {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_map(AggregationVisitor)
    }
}

struct AggregationVisitor;

impl<'de> Visitor<'de> for AggregationVisitor {
    type Value = Aggregation;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an Aggregation map with a `kind` tag")
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> Result<Self::Value, A::Error> {
        let kind = read_kind(&mut map)?;
        let out = match kind.as_str() {
            "count" => Aggregation::Count,
            "min" => Aggregation::Min {
                metric: read_named(&mut map, "metric")?,
            },
            "max" => Aggregation::Max {
                metric: read_named(&mut map, "metric")?,
            },
            "sum" => Aggregation::Sum {
                metric: read_named(&mut map, "metric")?,
            },
            "avg" => Aggregation::Avg {
                metric: read_named(&mut map, "metric")?,
            },
            "percentile" => {
                let metric =
                    read_named::<_, ElementParameterRef>(&mut map, "metric")?;
                let p = read_named::<_, f64>(&mut map, "p")?;
                Aggregation::Percentile { metric, p }
            }
            "group_by" => {
                let dimension =
                    read_named::<_, GroupDimension>(&mut map, "dimension")?;
                let then = read_named::<_, Box<Aggregation>>(&mut map, "then")?;
                Aggregation::GroupBy { dimension, then }
            }
            other => {
                return Err(A::Error::custom(format!(
                    "unknown Aggregation kind `{other}`"
                )));
            }
        };
        drain_rest(map)?;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// AggregateResult.
// ---------------------------------------------------------------------------

impl Serialize for AggregateResult {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut m = ser.serialize_map(None)?;
        match self {
            Self::Scalar { value } => {
                m.serialize_entry("kind", "scalar")?;
                m.serialize_entry("value", value)?;
            }
            Self::Count { n } => {
                m.serialize_entry("kind", "count")?;
                m.serialize_entry("n", n)?;
            }
            Self::Grouped { groups } => {
                m.serialize_entry("kind", "grouped")?;
                m.serialize_entry("groups", groups)?;
            }
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for AggregateResult {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_map(AggregateResultVisitor)
    }
}

struct AggregateResultVisitor;

impl<'de> Visitor<'de> for AggregateResultVisitor {
    type Value = AggregateResult;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an AggregateResult map with a `kind` tag")
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut map: A,
    ) -> Result<Self::Value, A::Error> {
        let kind = read_kind(&mut map)?;
        let out = match kind.as_str() {
            "scalar" => AggregateResult::Scalar {
                value: read_named(&mut map, "value")?,
            },
            "count" => AggregateResult::Count {
                n: read_named(&mut map, "n")?,
            },
            "grouped" => AggregateResult::Grouped {
                groups: read_named(&mut map, "groups")?,
            },
            other => {
                return Err(A::Error::custom(format!(
                    "unknown AggregateResult kind `{other}`"
                )));
            }
        };
        drain_rest(map)?;
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Visitor helpers.
// ---------------------------------------------------------------------------

/// Read the first entry; require the key `"kind"`, return the value.
fn read_kind<'de, A: MapAccess<'de>>(map: &mut A) -> Result<String, A::Error> {
    let key: String = map
        .next_key()?
        .ok_or_else(|| A::Error::custom("expected `kind` as the first entry"))?;
    if key != "kind" {
        return Err(A::Error::custom(format!(
            "expected `kind` first, got `{key}`"
        )));
    }
    map.next_value()
}

/// Read the next entry; require its key to equal `expected`, return the
/// deserialised value.
fn read_named<'de, A, T>(map: &mut A, expected: &'static str) -> Result<T, A::Error>
where
    A: MapAccess<'de>,
    T: Deserialize<'de>,
{
    let key: String = map.next_key()?.ok_or_else(|| {
        A::Error::custom(format!("missing expected field `{expected}`"))
    })?;
    if key != expected {
        return Err(A::Error::custom(format!(
            "expected field `{expected}`, got `{key}`"
        )));
    }
    map.next_value()
}

/// Drain any trailing entries (silently); tolerates forward-compatible
/// extra fields without failing deserialisation.
fn drain_rest<'de, A: MapAccess<'de>>(mut map: A) -> Result<(), A::Error> {
    while let Some(_key) = map.next_key::<String>()? {
        let _: serde::de::IgnoredAny = map.next_value()?;
    }
    Ok(())
}
