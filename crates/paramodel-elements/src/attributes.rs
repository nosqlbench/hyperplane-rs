// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Labels, tags, plugs, sockets, and wires.
//!
//! Three tiers per SRD-0005:
//!
//! - **Labels** — intrinsic facts ("what this entity *is*"). Participate
//!   in identity and fingerprinting.
//! - **Tags** — extrinsic organisation ("how I group/filter this").
//!   Do *not* participate in identity.
//! - **Plugs and sockets** — structured compatibility surface on
//!   elements. Plugs name the upstreams an element *needs*; sockets
//!   name the points where downstreams *connect to* it. Each carries a
//!   non-empty set of `(key, value)` facets; a plug fits a socket iff
//!   the socket's facets cover (superset) the plug's.
//!
//! Wires are the concrete plug↔socket connections the compiler produces
//! when it resolves an authored dependency — [`wiring_for`] returns the
//! [`WireMatch`] for a dependent's plugs against a target's sockets.
//!
//! Namespace-uniqueness across tiers is enforced by
//! [`validate_namespace`]: a single key cannot appear as both a label
//! key and a tag key, nor as both an attribute key and a port name.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize, de};

use crate::name_type;
use crate::names::NameError;

// ---------------------------------------------------------------------------
// AttributeError + Tier.
// ---------------------------------------------------------------------------

/// Tier a given key was authored on.
///
/// Used in diagnostics when a key is found in more than one tier at
/// once. `Port` covers both plug and socket names — they share the
/// per-element name namespace (SRD-0005 D13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Labels: intrinsic facts.
    Label,
    /// Tags: extrinsic organisation.
    Tag,
    /// Port (plug or socket) name.
    Port,
}

/// Errors raised by attribute-layer construction and validation.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AttributeError {
    /// The same key appears in multiple tiers on one entity.
    #[error("attribute key '{key}' appears in multiple tiers: {tiers:?}")]
    DuplicateKey {
        /// The conflicting key.
        key:   String,
        /// Every tier the key was seen in.
        tiers: Vec<Tier>,
    },

    /// A plug or socket declared no facets.
    #[error("port '{port}' has an empty facet set; at least one facet is required")]
    EmptyFacetSet {
        /// The offending port name.
        port: String,
    },

    /// Two plugs, two sockets, or a plug and a socket share a name.
    #[error("port name '{name}' is used more than once across plugs and sockets")]
    DuplicatePortName {
        /// The duplicated port name.
        name: String,
    },
}

// ---------------------------------------------------------------------------
// Attribute-value validation — distinct from Name validation because
// values are UTF-8 strings with control-character exclusion only, not
// identifier-safe ASCII. LabelValue and TagValue share this rule.
// ---------------------------------------------------------------------------

const ATTRIBUTE_VALUE_MAX_LEN: usize = 256;

fn validate_attribute_value(s: &str) -> Result<(), NameError> {
    if s.is_empty() {
        return Err(NameError::Empty);
    }
    if s.len() > ATTRIBUTE_VALUE_MAX_LEN {
        return Err(NameError::TooLong {
            length: s.len(),
            max:    ATTRIBUTE_VALUE_MAX_LEN,
        });
    }
    for (offset, ch) in s.char_indices() {
        if ch.is_control() {
            return Err(NameError::InvalidChar { ch, offset });
        }
    }
    Ok(())
}

/// Stamp out a validated attribute-value newtype (UTF-8, ≤ 256 bytes,
/// no control characters).
macro_rules! attribute_value_type {
    ($Name:ident, $kind:literal) => {
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $Name(String);

        impl $Name {
            /// Construct a new value, validating the candidate.
            pub fn new(candidate: impl Into<String>) -> std::result::Result<Self, NameError> {
                let s = candidate.into();
                validate_attribute_value(&s)?;
                Ok(Self(s))
            }

            /// Borrow the inner string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Consume and return the inner string.
            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl AsRef<str> for $Name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $Name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl std::fmt::Debug for $Name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({:?})", $kind, self.0)
            }
        }

        impl Serialize for $Name {
            fn serialize<S: serde::Serializer>(
                &self,
                s: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $Name {
            fn deserialize<D: serde::Deserializer<'de>>(
                d: D,
            ) -> std::result::Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                Self::new(s).map_err(de::Error::custom)
            }
        }
    };
}

attribute_value_type!(LabelValue, "LabelValue");
attribute_value_type!(TagValue, "TagValue");

// ---------------------------------------------------------------------------
// Name-style newtypes (ASCII identifier + '-' '.' '_').
// ---------------------------------------------------------------------------

name_type! {
    /// Intrinsic-fact key on an attributable entity. Canonical
    /// well-known keys live in [`label`].
    pub struct LabelKey { kind: "LabelKey" }
}

name_type! {
    /// Organisational tag key. Adopter-defined; paramodel ships no
    /// canonical keys at this layer.
    pub struct TagKey { kind: "TagKey" }
}

name_type! {
    /// Key half of a `(key, value)` facet pair on a plug or socket.
    pub struct FacetKey { kind: "FacetKey" }
}

name_type! {
    /// Value half of a `(key, value)` facet pair.
    pub struct FacetValue { kind: "FacetValue" }
}

name_type! {
    /// Shared name for plugs and sockets. The two kinds live in the
    /// same per-element namespace (SRD-0005 D13): an element may not
    /// have a plug and a socket with the same `PortName`.
    pub struct PortName { kind: "PortName" }
}

// ---------------------------------------------------------------------------
// Well-known label keys.
// ---------------------------------------------------------------------------

/// Canonical label keys. Not *required* on every entity — authoring
/// these spellings lets tooling treat them uniformly.
pub mod label {
    use super::LabelKey;

    /// `"name"` — the human-readable name of the entity.
    #[must_use]
    pub fn name() -> LabelKey {
        LabelKey::new("name").expect("`name` is a valid label key")
    }

    /// `"type"` — the entity's role / kind.
    #[must_use]
    pub fn r#type() -> LabelKey {
        LabelKey::new("type").expect("`type` is a valid label key")
    }

    /// `"description"` — a free-form description of the entity.
    #[must_use]
    pub fn description() -> LabelKey {
        LabelKey::new("description").expect("`description` is a valid label key")
    }
}

// ---------------------------------------------------------------------------
// Labels and Tags.
// ---------------------------------------------------------------------------

/// Intrinsic-fact map. See module docs and SRD-0005.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Labels(BTreeMap<LabelKey, LabelValue>);

impl Labels {
    /// Empty label map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a label. Returns the previous value for `key`, if any.
    pub fn insert(&mut self, key: LabelKey, value: LabelValue) -> Option<LabelValue> {
        self.0.insert(key, value)
    }

    /// Look up a label.
    #[must_use]
    pub fn get(&self, key: &LabelKey) -> Option<&LabelValue> {
        self.0.get(key)
    }

    /// Sorted-by-key iterator over `(key, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&LabelKey, &LabelValue)> {
        self.0.iter()
    }

    /// Key-only iterator, sorted.
    pub fn keys(&self) -> impl Iterator<Item = &LabelKey> {
        self.0.keys()
    }

    /// Number of labels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when no labels are set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Key-membership test.
    #[must_use]
    pub fn contains_key(&self, key: &LabelKey) -> bool {
        self.0.contains_key(key)
    }
}

impl FromIterator<(LabelKey, LabelValue)> for Labels {
    fn from_iter<I: IntoIterator<Item = (LabelKey, LabelValue)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// Extrinsic-organisation map. See module docs and SRD-0005.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tags(BTreeMap<TagKey, TagValue>);

impl Tags {
    /// Empty tag map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a tag. Returns the previous value for `key`, if any.
    pub fn insert(&mut self, key: TagKey, value: TagValue) -> Option<TagValue> {
        self.0.insert(key, value)
    }

    /// Look up a tag.
    #[must_use]
    pub fn get(&self, key: &TagKey) -> Option<&TagValue> {
        self.0.get(key)
    }

    /// Sorted-by-key iterator over `(key, value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&TagKey, &TagValue)> {
        self.0.iter()
    }

    /// Key-only iterator, sorted.
    pub fn keys(&self) -> impl Iterator<Item = &TagKey> {
        self.0.keys()
    }

    /// Number of tags.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// `true` when no tags are set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Key-membership test.
    #[must_use]
    pub fn contains_key(&self, key: &TagKey) -> bool {
        self.0.contains_key(key)
    }
}

impl FromIterator<(TagKey, TagValue)> for Tags {
    fn from_iter<I: IntoIterator<Item = (TagKey, TagValue)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

// ---------------------------------------------------------------------------
// Facet, Plug, Socket.
// ---------------------------------------------------------------------------

/// Structured `(key, value)` pair on a plug or socket. Canonical human
/// rendering is `"key:value"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Facet {
    /// Dimension of classification.
    pub key:   FacetKey,
    /// Point on that dimension.
    pub value: FacetValue,
}

impl Facet {
    /// Construct a new facet.
    pub fn new(
        key:   impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, NameError> {
        Ok(Self {
            key:   FacetKey::new(key)?,
            value: FacetValue::new(value)?,
        })
    }
}

impl std::fmt::Display for Facet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.key, self.value)
    }
}

/// A point where an element needs to connect to an upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plug {
    /// Name of this plug on the owning element.
    pub name:        PortName,
    /// Facets that any fitting socket must cover.
    pub facets:      BTreeSet<Facet>,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Plug {
    /// Construct a plug. Rejects an empty facet set per SRD-0005 D8.
    pub fn new(name: PortName, facets: BTreeSet<Facet>) -> Result<Self, AttributeError> {
        if facets.is_empty() {
            return Err(AttributeError::EmptyFacetSet {
                port: name.into_inner(),
            });
        }
        Ok(Self {
            name,
            facets,
            description: None,
        })
    }

    /// Builder-style setter for the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// This plug fits the given socket iff the socket's facet set is a
    /// superset of the plug's.
    #[must_use]
    pub fn fits(&self, socket: &Socket) -> bool {
        socket.facets.is_superset(&self.facets)
    }
}

/// A point where downstream elements connect to this element.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Socket {
    /// Name of this socket on the owning element.
    pub name:        PortName,
    /// Facets this socket covers.
    pub facets:      BTreeSet<Facet>,
    /// Optional human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Socket {
    /// Construct a socket. Rejects an empty facet set per SRD-0005 D8.
    pub fn new(name: PortName, facets: BTreeSet<Facet>) -> Result<Self, AttributeError> {
        if facets.is_empty() {
            return Err(AttributeError::EmptyFacetSet {
                port: name.into_inner(),
            });
        }
        Ok(Self {
            name,
            facets,
            description: None,
        })
    }

    /// Builder-style setter for the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Does the plug fit the socket? True iff `socket.facets` is a
/// superset of `plug.facets`.
#[must_use]
pub fn fits(plug: &Plug, socket: &Socket) -> bool {
    plug.fits(socket)
}

// ---------------------------------------------------------------------------
// Wire, WireMatch, wiring_for.
// ---------------------------------------------------------------------------

/// One concrete plug↔socket connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Wire {
    /// The plug on the dependent element.
    pub plug:   PortName,
    /// The socket on the target element.
    pub socket: PortName,
}

/// Outcome of matching a dependent element's plugs against a target
/// element's sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireMatch {
    /// Every plug on the dependent has exactly one fitting socket on
    /// the target.
    Complete {
        /// Resolved connections.
        wires: Vec<Wire>,
    },
    /// Mixed outcome: some plugs wired, others unfitted or ambiguous.
    Partial {
        /// Plugs with exactly one fitting socket.
        wires:     Vec<Wire>,
        /// Plugs with no fitting socket.
        unfitted:  Vec<PortName>,
        /// Plugs with more than one fitting socket — each entry names
        /// the plug and the candidate socket list.
        ambiguous: Vec<(PortName, Vec<PortName>)>,
    },
    /// The dependent has at least one plug and none of its plugs have
    /// any fitting socket on the target.
    None,
}

/// Match each plug on the dependent against the target's sockets,
/// returning a [`WireMatch`] describing the outcome.
///
/// Per SRD-0005 D15, a plug with more than one fitting socket is an
/// authoring error — the caller (the compiler) escalates an `Ambiguous`
/// entry into a compile failure.
#[must_use]
pub fn wiring_for(dependent_plugs: &[Plug], target_sockets: &[Socket]) -> WireMatch {
    let mut wires = Vec::new();
    let mut unfitted = Vec::new();
    let mut ambiguous = Vec::new();

    for plug in dependent_plugs {
        let fitting: Vec<&Socket> = target_sockets
            .iter()
            .filter(|s| plug.fits(s))
            .collect();
        match fitting.len() {
            0 => unfitted.push(plug.name.clone()),
            1 => wires.push(Wire {
                plug:   plug.name.clone(),
                socket: fitting[0].name.clone(),
            }),
            _ => {
                let candidates: Vec<PortName> =
                    fitting.iter().map(|s| s.name.clone()).collect();
                ambiguous.push((plug.name.clone(), candidates));
            }
        }
    }

    if dependent_plugs.is_empty() || (unfitted.is_empty() && ambiguous.is_empty()) {
        WireMatch::Complete { wires }
    } else if wires.is_empty() && ambiguous.is_empty() {
        WireMatch::None
    } else {
        WireMatch::Partial {
            wires,
            unfitted,
            ambiguous,
        }
    }
}

// ---------------------------------------------------------------------------
// Namespace validation across tiers.
// ---------------------------------------------------------------------------

/// Validate the three-tier namespace-uniqueness rule (SRD-0005 D5).
///
/// Enforces:
///
/// - Port names are unique across plugs and sockets on one element.
/// - No key appears in more than one tier (label / tag / port).
pub fn validate_namespace(
    labels:  &Labels,
    tags:    &Tags,
    plugs:   &[Plug],
    sockets: &[Socket],
) -> Result<(), AttributeError> {
    // 1. Port names must be unique across plugs and sockets.
    let mut port_names: BTreeSet<String> = BTreeSet::new();
    for plug in plugs {
        if !port_names.insert(plug.name.as_str().to_owned()) {
            return Err(AttributeError::DuplicatePortName {
                name: plug.name.as_str().to_owned(),
            });
        }
    }
    for socket in sockets {
        if !port_names.insert(socket.name.as_str().to_owned()) {
            return Err(AttributeError::DuplicatePortName {
                name: socket.name.as_str().to_owned(),
            });
        }
    }

    // 2. A single key may not appear in more than one tier.
    let mut by_tier: BTreeMap<String, BTreeSet<Tier>> = BTreeMap::new();
    for key in labels.keys() {
        by_tier
            .entry(key.as_str().to_owned())
            .or_default()
            .insert(Tier::Label);
    }
    for key in tags.keys() {
        by_tier
            .entry(key.as_str().to_owned())
            .or_default()
            .insert(Tier::Tag);
    }
    for port in &port_names {
        by_tier.entry(port.clone()).or_default().insert(Tier::Port);
    }

    for (key, tiers) in by_tier {
        if tiers.len() > 1 {
            return Err(AttributeError::DuplicateKey {
                key,
                tiers: tiers.into_iter().collect(),
            });
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Attributed / Pluggable traits.
// ---------------------------------------------------------------------------

/// Read interface for any entity carrying [`Labels`] and [`Tags`].
pub trait Attributed {
    /// Intrinsic facts.
    fn labels(&self) -> &Labels;

    /// Extrinsic organisation.
    fn tags(&self) -> &Tags;
}

/// Read interface for elements — adds plugs and sockets on top of
/// [`Attributed`]. Only elements implement `Pluggable` at this layer
/// (SRD-0005 D11).
pub trait Pluggable: Attributed {
    /// The element's plugs.
    fn plugs(&self) -> &[Plug];

    /// The element's sockets.
    fn sockets(&self) -> &[Socket];
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn lk(s: &str) -> LabelKey {
        LabelKey::new(s).unwrap()
    }
    fn lv(s: &str) -> LabelValue {
        LabelValue::new(s).unwrap()
    }
    fn tk(s: &str) -> TagKey {
        TagKey::new(s).unwrap()
    }
    fn tv(s: &str) -> TagValue {
        TagValue::new(s).unwrap()
    }
    fn pn(s: &str) -> PortName {
        PortName::new(s).unwrap()
    }
    fn facet(k: &str, v: &str) -> Facet {
        Facet::new(k, v).unwrap()
    }
    fn fset(pairs: &[(&str, &str)]) -> BTreeSet<Facet> {
        pairs.iter().map(|(k, v)| facet(k, v)).collect()
    }

    // ---------- LabelValue / TagValue validation ----------

    #[test]
    fn label_value_accepts_utf8_and_rejects_control_and_overlong() {
        assert!(LabelValue::new("hello world").is_ok());
        assert!(LabelValue::new("日本語").is_ok());
        assert!(LabelValue::new("").is_err());
        assert!(LabelValue::new("has\ncontrol").is_err());
        let overlong = "x".repeat(ATTRIBUTE_VALUE_MAX_LEN + 1);
        assert!(LabelValue::new(overlong).is_err());
    }

    #[test]
    fn label_value_serde_roundtrip() {
        let v = LabelValue::new("service").unwrap();
        let json = serde_json::to_string(&v).unwrap();
        assert_eq!(json, "\"service\"");
        let back: LabelValue = serde_json::from_str(&json).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn tag_value_mirrors_label_value_rules() {
        assert!(TagValue::new("staging").is_ok());
        assert!(TagValue::new("").is_err());
        assert!(TagValue::new("ctrl\tchar").is_err());
    }

    // ---------- Name-style newtypes ----------

    #[test]
    fn label_and_tag_keys_share_name_rules() {
        LabelKey::new("type").unwrap();
        TagKey::new("owner").unwrap();
        assert!(LabelKey::new("1starts").is_err());
        assert!(TagKey::new("has space").is_err());
    }

    #[test]
    fn facet_key_and_value_validation() {
        assert!(Facet::new("api", "rest").is_ok());
        assert!(Facet::new("", "x").is_err());
        assert!(Facet::new("k", "").is_err());
    }

    #[test]
    fn port_name_validates() {
        assert!(PortName::new("vector_service").is_ok());
        assert!(PortName::new("db-main").is_ok());
        assert!(PortName::new("").is_err());
    }

    #[test]
    fn facet_display_is_key_colon_value() {
        let f = Facet::new("api", "rest").unwrap();
        assert_eq!(format!("{f}"), "api:rest");
    }

    // ---------- label:: module ----------

    #[test]
    fn well_known_label_keys_are_valid() {
        assert_eq!(label::name().as_str(), "name");
        assert_eq!(label::r#type().as_str(), "type");
        assert_eq!(label::description().as_str(), "description");
    }

    // ---------- Labels / Tags ----------

    #[test]
    fn labels_insert_and_query() {
        let mut ls = Labels::new();
        ls.insert(lk("type"), lv("service"));
        assert_eq!(ls.len(), 1);
        assert!(!ls.is_empty());
        assert!(ls.contains_key(&lk("type")));
        assert_eq!(ls.get(&lk("type")), Some(&lv("service")));
    }

    #[test]
    fn labels_iter_is_sorted_by_key() {
        let mut ls = Labels::new();
        ls.insert(lk("zebra"), lv("z"));
        ls.insert(lk("apple"), lv("a"));
        ls.insert(lk("mango"), lv("m"));
        let keys: Vec<&str> = ls.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["apple", "mango", "zebra"]);
    }

    #[test]
    fn tags_from_iterator() {
        let t: Tags = [(tk("owner"), tv("jshook")), (tk("env"), tv("staging"))]
            .into_iter()
            .collect();
        assert_eq!(t.len(), 2);
        assert_eq!(t.get(&tk("owner")), Some(&tv("jshook")));
    }

    #[test]
    fn labels_serde_roundtrip() {
        let mut ls = Labels::new();
        ls.insert(lk("type"), lv("service"));
        let json = serde_json::to_string(&ls).unwrap();
        // Serializes transparently as a JSON object keyed by LabelKey.
        assert_eq!(json, "{\"type\":\"service\"}");
        let back: Labels = serde_json::from_str(&json).unwrap();
        assert_eq!(ls, back);
    }

    // ---------- Plug / Socket / fits ----------

    #[test]
    fn plug_rejects_empty_facets() {
        let err = Plug::new(pn("p"), BTreeSet::new()).unwrap_err();
        assert!(matches!(err, AttributeError::EmptyFacetSet { .. }));
    }

    #[test]
    fn socket_rejects_empty_facets() {
        let err = Socket::new(pn("s"), BTreeSet::new()).unwrap_err();
        assert!(matches!(err, AttributeError::EmptyFacetSet { .. }));
    }

    #[test]
    fn plug_fits_when_socket_covers_facets() {
        let plug = Plug::new(
            pn("vector_service"),
            fset(&[("api", "rest"), ("protocol", "vectorbench"), ("index", "hnsw")]),
        )
        .unwrap();
        let good_socket = Socket::new(
            pn("api"),
            fset(&[
                ("api", "rest"),
                ("protocol", "vectorbench"),
                ("index", "hnsw"),
                ("index", "diskann"),
                ("runtime", "jvm"),
            ]),
        )
        .unwrap();
        assert!(plug.fits(&good_socket));
        assert!(fits(&plug, &good_socket));
    }

    #[test]
    fn plug_fails_to_fit_missing_facet() {
        let plug = Plug::new(
            pn("vector_service"),
            fset(&[("api", "rest"), ("index", "hnsw")]),
        )
        .unwrap();
        let wrong_index = Socket::new(
            pn("api"),
            fset(&[("api", "rest"), ("index", "ivf"), ("index", "flat")]),
        )
        .unwrap();
        assert!(!plug.fits(&wrong_index));
    }

    #[test]
    fn socket_can_cover_multiple_values_for_same_key() {
        let plug = Plug::new(pn("db"), fset(&[("engine", "postgres-15")])).unwrap();
        let socket = Socket::new(
            pn("db_rw"),
            fset(&[("engine", "postgres"), ("engine", "postgres-15")]),
        )
        .unwrap();
        assert!(plug.fits(&socket));
    }

    #[test]
    fn plug_serde_roundtrip() {
        let plug = Plug::new(pn("db"), fset(&[("engine", "postgres")]))
            .unwrap()
            .with_description("primary datastore");
        let json = serde_json::to_string(&plug).unwrap();
        let back: Plug = serde_json::from_str(&json).unwrap();
        assert_eq!(plug, back);
    }

    // ---------- wiring_for / WireMatch ----------

    #[test]
    fn wiring_for_complete_match() {
        let plug = Plug::new(pn("vector_service"), fset(&[("api", "rest")])).unwrap();
        let socket = Socket::new(
            pn("api"),
            fset(&[("api", "rest"), ("runtime", "jvm")]),
        )
        .unwrap();
        let result = wiring_for(&[plug], &[socket]);
        match result {
            WireMatch::Complete { wires } => {
                assert_eq!(wires.len(), 1);
                assert_eq!(wires[0].plug.as_str(), "vector_service");
                assert_eq!(wires[0].socket.as_str(), "api");
            }
            other => panic!("expected Complete, got {other:?}"),
        }
    }

    #[test]
    fn wiring_for_none_when_no_sockets_fit_any_plug() {
        let plug = Plug::new(pn("db"), fset(&[("engine", "postgres")])).unwrap();
        let socket = Socket::new(pn("api"), fset(&[("engine", "mysql")])).unwrap();
        let result = wiring_for(&[plug], &[socket]);
        assert_eq!(result, WireMatch::None);
    }

    #[test]
    fn wiring_for_partial_on_mixed_outcome() {
        let wired = Plug::new(pn("api"), fset(&[("api", "rest")])).unwrap();
        let unfitted = Plug::new(pn("queue"), fset(&[("protocol", "amqp")])).unwrap();
        let socket = Socket::new(pn("api_sock"), fset(&[("api", "rest")])).unwrap();
        let result = wiring_for(&[wired, unfitted], &[socket]);
        match result {
            WireMatch::Partial {
                wires,
                unfitted,
                ambiguous,
            } => {
                assert_eq!(wires.len(), 1);
                assert_eq!(unfitted, vec![pn("queue")]);
                assert!(ambiguous.is_empty());
            }
            other => panic!("expected Partial, got {other:?}"),
        }
    }

    #[test]
    fn wiring_for_flags_ambiguous_plug_against_multiple_sockets() {
        // The SRD-0005 D15 example: a `db` plug that fits both
        // `read_pool` and `write_pool` sockets.
        let plug = Plug::new(
            pn("db"),
            fset(&[("kind", "database"), ("engine", "postgres")]),
        )
        .unwrap();
        let read_pool = Socket::new(
            pn("read_pool"),
            fset(&[
                ("kind", "database"),
                ("engine", "postgres"),
                ("access", "readonly"),
            ]),
        )
        .unwrap();
        let write_pool = Socket::new(
            pn("write_pool"),
            fset(&[
                ("kind", "database"),
                ("engine", "postgres"),
                ("access", "readwrite"),
            ]),
        )
        .unwrap();
        let result = wiring_for(&[plug], &[read_pool, write_pool]);
        match result {
            WireMatch::Partial { ambiguous, .. } => {
                assert_eq!(ambiguous.len(), 1);
                let (plug_name, candidates) = &ambiguous[0];
                assert_eq!(plug_name.as_str(), "db");
                assert_eq!(candidates.len(), 2);
            }
            other => panic!("expected Partial with ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn wiring_for_empty_plugs_is_trivially_complete() {
        let socket = Socket::new(pn("api"), fset(&[("api", "rest")])).unwrap();
        assert_eq!(
            wiring_for(&[], &[socket]),
            WireMatch::Complete { wires: vec![] }
        );
    }

    // ---------- validate_namespace ----------

    #[test]
    fn validate_namespace_accepts_disjoint_keys() {
        let mut labels = Labels::new();
        labels.insert(lk("type"), lv("service"));
        let mut tags = Tags::new();
        tags.insert(tk("owner"), tv("jshook"));
        let plugs = vec![Plug::new(pn("db"), fset(&[("engine", "postgres")])).unwrap()];
        let sockets =
            vec![Socket::new(pn("api"), fset(&[("api", "rest")])).unwrap()];
        assert!(validate_namespace(&labels, &tags, &plugs, &sockets).is_ok());
    }

    #[test]
    fn validate_namespace_rejects_label_tag_collision() {
        let mut labels = Labels::new();
        labels.insert(lk("type"), lv("service"));
        let mut tags = Tags::new();
        tags.insert(tk("type"), tv("whatever"));
        let err = validate_namespace(&labels, &tags, &[], &[]).unwrap_err();
        match err {
            AttributeError::DuplicateKey { key, tiers } => {
                assert_eq!(key, "type");
                assert!(tiers.contains(&Tier::Label));
                assert!(tiers.contains(&Tier::Tag));
            }
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn validate_namespace_rejects_label_port_collision() {
        let mut labels = Labels::new();
        labels.insert(lk("api"), lv("x"));
        let plugs = vec![Plug::new(pn("api"), fset(&[("x", "y")])).unwrap()];
        let err = validate_namespace(&labels, &Tags::new(), &plugs, &[]).unwrap_err();
        assert!(matches!(err, AttributeError::DuplicateKey { .. }));
    }

    #[test]
    fn validate_namespace_rejects_duplicate_port_name() {
        let plug = Plug::new(pn("api"), fset(&[("x", "y")])).unwrap();
        let socket = Socket::new(pn("api"), fset(&[("x", "y")])).unwrap();
        let err =
            validate_namespace(&Labels::new(), &Tags::new(), &[plug], &[socket])
                .unwrap_err();
        match err {
            AttributeError::DuplicatePortName { name } => assert_eq!(name, "api"),
            other => panic!("wrong error: {other:?}"),
        }
    }

    // ---------- Attributed / Pluggable traits ----------

    struct Dummy {
        labels:  Labels,
        tags:    Tags,
        plugs:   Vec<Plug>,
        sockets: Vec<Socket>,
    }

    impl Attributed for Dummy {
        fn labels(&self) -> &Labels {
            &self.labels
        }
        fn tags(&self) -> &Tags {
            &self.tags
        }
    }

    impl Pluggable for Dummy {
        fn plugs(&self) -> &[Plug] {
            &self.plugs
        }
        fn sockets(&self) -> &[Socket] {
            &self.sockets
        }
    }

    #[test]
    fn traits_read_through() {
        let d = Dummy {
            labels:  Labels::new(),
            tags:    Tags::new(),
            plugs:   vec![Plug::new(pn("api"), fset(&[("x", "y")])).unwrap()],
            sockets: vec![],
        };
        assert_eq!(<Dummy as Pluggable>::plugs(&d).len(), 1);
        assert!(<Dummy as Attributed>::labels(&d).is_empty());
    }
}
