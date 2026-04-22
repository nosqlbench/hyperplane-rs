// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Parameters.
//!
//! The five concrete parameter kinds from SRD-0004 — `IntegerParameter`,
//! `DoubleParameter`, `BooleanParameter`, `StringParameter`,
//! `SelectionParameter` — plus `DerivedParameter` (values computed from
//! an [`Expression`]). The outer [`Parameter`] enum dispatches name,
//! kind, labels, tags, default, generation, and validation across
//! variants.
//!
//! Constructors validate the inputs (range order, domain non-emptiness,
//! max-selections ≥ 1, …). Builder-style setters (`with_default`,
//! `with_constraint`, `with_label`, `with_tag`) return
//! `Result<Self>` when they can fail — `with_default` rejects a default
//! outside the domain or violating a registered constraint;
//! `with_label`/`with_tag` reject keys that already live on the other
//! tier (namespace uniqueness per SRD-0005 D5).

use std::collections::BTreeSet;

use indexmap::IndexSet;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};

use crate::attributes::{AttributeError, Attributed, LabelKey, LabelValue, Labels, TagKey, TagValue, Tags, Tier};
use crate::constraint::{
    BoolConstraint, Constraint, DoubleConstraint, IntConstraint, SelectionConstraint,
    StringConstraint,
};
use crate::domain::{
    Domain, DoubleDomain, IntegerDomain, ResolverId, SelectionDomain, StringDomain,
};
use crate::expression::{DerivationError, EvalValue, Expression, ValueBindings};
use crate::names::ParameterName;
use crate::validation::ValidationResult;
use crate::value::{GeneratorInfo, SelectionItem, Value, ValueKind};

// ---------------------------------------------------------------------------
// ParameterError.
// ---------------------------------------------------------------------------

/// Errors from parameter construction and builder-style setters.
#[derive(Debug, thiserror::Error)]
pub enum ParameterError {
    /// `with_default` was given a value outside the domain.
    #[error("default value is not in the parameter's domain")]
    DefaultNotInDomain,

    /// `with_default` was given a value that fails a registered constraint.
    #[error("default value violates a registered constraint")]
    DefaultViolatesConstraint,

    /// A constraint's kind differs from the parameter's.
    #[error("constraint kind does not match parameter kind ({parameter_kind:?})")]
    ConstraintKindMismatch {
        /// The parameter's kind.
        parameter_kind: ValueKind,
    },

    /// A `DerivedParameter` was constructed with a Selection kind
    /// (expressions only produce integer/double/boolean/string).
    #[error("derived parameters cannot produce Selection values")]
    DerivedSelectionUnsupported,

    /// `with_label` or `with_tag` introduced a key already present in
    /// another tier on this parameter.
    #[error(transparent)]
    Attribute(#[from] AttributeError),
}

// ---------------------------------------------------------------------------
// IntegerParameter.
// ---------------------------------------------------------------------------

/// A parameter that observes an `i64`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegerParameter {
    /// Parameter name.
    pub name:        ParameterName,
    /// Value domain.
    pub domain:      IntegerDomain,
    /// Registered constraints; all must hold.
    #[serde(default)]
    pub constraints: Vec<IntConstraint>,
    /// Optional default value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default:     Option<i64>,
    /// Intrinsic-fact map.
    #[serde(default)]
    pub labels:      Labels,
    /// Organisational tag map.
    #[serde(default)]
    pub tags:        Tags,
}

impl IntegerParameter {
    /// Construct a parameter backed by an inclusive numeric range.
    pub fn range(name: ParameterName, min: i64, max: i64) -> crate::Result<Self> {
        Ok(Self {
            name,
            domain: IntegerDomain::range(min, max)?,
            constraints: Vec::new(),
            default: None,
            labels: Labels::new(),
            tags: Tags::new(),
        })
    }

    /// Construct a parameter backed by a discrete value set.
    pub fn of(name: ParameterName, values: BTreeSet<i64>) -> crate::Result<Self> {
        Ok(Self {
            name,
            domain: IntegerDomain::discrete(values)?,
            constraints: Vec::new(),
            default: None,
            labels: Labels::new(),
            tags: Tags::new(),
        })
    }

    /// Set the default value. Rejects out-of-domain defaults and
    /// defaults that violate registered constraints.
    pub fn with_default(mut self, default: i64) -> crate::Result<Self> {
        if !self.domain.contains_native(default) {
            return Err(ParameterError::DefaultNotInDomain.into());
        }
        for c in &self.constraints {
            if !c.test(default) {
                return Err(ParameterError::DefaultViolatesConstraint.into());
            }
        }
        self.default = Some(default);
        Ok(self)
    }

    /// Append a constraint.
    #[must_use]
    pub fn with_constraint(mut self, c: IntConstraint) -> Self {
        self.constraints.push(c);
        self
    }

    /// Add a label. Rejects keys already used as a tag on this parameter.
    pub fn with_label(mut self, key: LabelKey, value: LabelValue) -> crate::Result<Self> {
        check_no_tag_conflict(&self.tags, key.as_str())?;
        self.labels.insert(key, value);
        Ok(self)
    }

    /// Add a tag. Rejects keys already used as a label on this parameter.
    pub fn with_tag(mut self, key: TagKey, value: TagValue) -> crate::Result<Self> {
        check_no_label_conflict(&self.labels, key.as_str())?;
        self.tags.insert(key, value);
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// DoubleParameter.
// ---------------------------------------------------------------------------

/// A parameter that observes an `f64`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoubleParameter {
    /// Parameter name.
    pub name:        ParameterName,
    /// Value domain.
    pub domain:      DoubleDomain,
    /// Registered constraints.
    #[serde(default)]
    pub constraints: Vec<DoubleConstraint>,
    /// Optional default value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default:     Option<f64>,
    /// Intrinsic facts.
    #[serde(default)]
    pub labels:      Labels,
    /// Organisational tags.
    #[serde(default)]
    pub tags:        Tags,
}

impl DoubleParameter {
    /// Construct a parameter backed by an inclusive range.
    pub fn range(name: ParameterName, min: f64, max: f64) -> crate::Result<Self> {
        Ok(Self {
            name,
            domain: DoubleDomain::range(min, max)?,
            constraints: Vec::new(),
            default: None,
            labels: Labels::new(),
            tags: Tags::new(),
        })
    }

    /// Set the default value. Rejects out-of-domain defaults and
    /// defaults that violate registered constraints.
    pub fn with_default(mut self, default: f64) -> crate::Result<Self> {
        if !self.domain.contains_native(default) {
            return Err(ParameterError::DefaultNotInDomain.into());
        }
        for c in &self.constraints {
            if !c.test(default) {
                return Err(ParameterError::DefaultViolatesConstraint.into());
            }
        }
        self.default = Some(default);
        Ok(self)
    }

    /// Append a constraint.
    #[must_use]
    pub fn with_constraint(mut self, c: DoubleConstraint) -> Self {
        self.constraints.push(c);
        self
    }

    /// Add a label. Rejects keys already used as a tag.
    pub fn with_label(mut self, key: LabelKey, value: LabelValue) -> crate::Result<Self> {
        check_no_tag_conflict(&self.tags, key.as_str())?;
        self.labels.insert(key, value);
        Ok(self)
    }

    /// Add a tag. Rejects keys already used as a label.
    pub fn with_tag(mut self, key: TagKey, value: TagValue) -> crate::Result<Self> {
        check_no_label_conflict(&self.labels, key.as_str())?;
        self.tags.insert(key, value);
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// BooleanParameter.
// ---------------------------------------------------------------------------

/// A parameter that observes a `bool`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BooleanParameter {
    /// Parameter name.
    pub name:        ParameterName,
    /// Registered constraints.
    #[serde(default)]
    pub constraints: Vec<BoolConstraint>,
    /// Optional default value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default:     Option<bool>,
    /// Intrinsic facts.
    #[serde(default)]
    pub labels:      Labels,
    /// Organisational tags.
    #[serde(default)]
    pub tags:        Tags,
}

impl BooleanParameter {
    /// Construct a new boolean parameter.
    #[must_use]
    pub fn of(name: ParameterName) -> Self {
        Self {
            name,
            constraints: Vec::new(),
            default: None,
            labels: Labels::new(),
            tags: Tags::new(),
        }
    }

    /// Set the default. Rejects if a registered constraint forbids it.
    pub fn with_default(mut self, default: bool) -> crate::Result<Self> {
        for c in &self.constraints {
            if !c.test(default) {
                return Err(ParameterError::DefaultViolatesConstraint.into());
            }
        }
        self.default = Some(default);
        Ok(self)
    }

    /// Append a constraint.
    #[must_use]
    pub fn with_constraint(mut self, c: BoolConstraint) -> Self {
        self.constraints.push(c);
        self
    }

    /// Add a label.
    pub fn with_label(mut self, key: LabelKey, value: LabelValue) -> crate::Result<Self> {
        check_no_tag_conflict(&self.tags, key.as_str())?;
        self.labels.insert(key, value);
        Ok(self)
    }

    /// Add a tag.
    pub fn with_tag(mut self, key: TagKey, value: TagValue) -> crate::Result<Self> {
        check_no_label_conflict(&self.labels, key.as_str())?;
        self.tags.insert(key, value);
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// StringParameter.
// ---------------------------------------------------------------------------

/// A parameter that observes a `String`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StringParameter {
    /// Parameter name.
    pub name:        ParameterName,
    /// Value domain (any string or regex-matched).
    pub domain:      StringDomain,
    /// Registered constraints.
    #[serde(default)]
    pub constraints: Vec<StringConstraint>,
    /// Optional default value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default:     Option<String>,
    /// Intrinsic facts.
    #[serde(default)]
    pub labels:      Labels,
    /// Organisational tags.
    #[serde(default)]
    pub tags:        Tags,
}

impl StringParameter {
    /// Construct a parameter accepting any string.
    #[must_use]
    pub fn of(name: ParameterName) -> Self {
        Self {
            name,
            domain: StringDomain::any(),
            constraints: Vec::new(),
            default: None,
            labels: Labels::new(),
            tags: Tags::new(),
        }
    }

    /// Construct a parameter restricted to strings matching `pattern`.
    pub fn regex(name: ParameterName, pattern: impl Into<String>) -> crate::Result<Self> {
        Ok(Self {
            name,
            domain: StringDomain::regex(pattern)?,
            constraints: Vec::new(),
            default: None,
            labels: Labels::new(),
            tags: Tags::new(),
        })
    }

    /// Set the default value. Rejects out-of-domain defaults and
    /// defaults that violate registered constraints.
    pub fn with_default(mut self, default: impl Into<String>) -> crate::Result<Self> {
        let default = default.into();
        if !self.domain.contains_native(&default) {
            return Err(ParameterError::DefaultNotInDomain.into());
        }
        for c in &self.constraints {
            if !c.test(&default) {
                return Err(ParameterError::DefaultViolatesConstraint.into());
            }
        }
        self.default = Some(default);
        Ok(self)
    }

    /// Append a constraint.
    #[must_use]
    pub fn with_constraint(mut self, c: StringConstraint) -> Self {
        self.constraints.push(c);
        self
    }

    /// Add a label.
    pub fn with_label(mut self, key: LabelKey, value: LabelValue) -> crate::Result<Self> {
        check_no_tag_conflict(&self.tags, key.as_str())?;
        self.labels.insert(key, value);
        Ok(self)
    }

    /// Add a tag.
    pub fn with_tag(mut self, key: TagKey, value: TagValue) -> crate::Result<Self> {
        check_no_label_conflict(&self.labels, key.as_str())?;
        self.tags.insert(key, value);
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// SelectionParameter.
// ---------------------------------------------------------------------------

/// A parameter whose value is an ordered multi-item selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionParameter {
    /// Parameter name.
    pub name:        ParameterName,
    /// Value domain (inline values or an external resolver).
    pub domain:      SelectionDomain,
    /// Registered constraints.
    #[serde(default)]
    pub constraints: Vec<SelectionConstraint>,
    /// Optional default selection (must be a subset of the domain).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default:     Option<IndexSet<SelectionItem>>,
    /// Intrinsic facts.
    #[serde(default)]
    pub labels:      Labels,
    /// Organisational tags.
    #[serde(default)]
    pub tags:        Tags,
}

impl SelectionParameter {
    /// Construct a parameter backed by an inline value set.
    pub fn of(
        name:           ParameterName,
        values:         IndexSet<SelectionItem>,
        max_selections: u32,
    ) -> crate::Result<Self> {
        Ok(Self {
            name,
            domain: SelectionDomain::fixed(values, max_selections)?,
            constraints: Vec::new(),
            default: None,
            labels: Labels::new(),
            tags: Tags::new(),
        })
    }

    /// Construct a parameter backed by an external resolver.
    pub fn external(
        name:           ParameterName,
        resolver:       ResolverId,
        max_selections: u32,
    ) -> crate::Result<Self> {
        Ok(Self {
            name,
            domain: SelectionDomain::external(resolver, max_selections)?,
            constraints: Vec::new(),
            default: None,
            labels: Labels::new(),
            tags: Tags::new(),
        })
    }

    /// Set the default selection.
    pub fn with_default(
        mut self,
        default: IndexSet<SelectionItem>,
    ) -> crate::Result<Self> {
        // Fixed domain: every item must be a legal member, and count must
        // be ≤ max_selections. External domains can't be shape-checked
        // without a resolver, so we accept any default there.
        if matches!(self.domain, SelectionDomain::Fixed { .. })
            && !self.domain.contains_items_fixed(&default)
        {
            return Err(ParameterError::DefaultNotInDomain.into());
        }
        for c in &self.constraints {
            if !c.test(&default) {
                return Err(ParameterError::DefaultViolatesConstraint.into());
            }
        }
        self.default = Some(default);
        Ok(self)
    }

    /// Append a constraint.
    #[must_use]
    pub fn with_constraint(mut self, c: SelectionConstraint) -> Self {
        self.constraints.push(c);
        self
    }

    /// Add a label.
    pub fn with_label(mut self, key: LabelKey, value: LabelValue) -> crate::Result<Self> {
        check_no_tag_conflict(&self.tags, key.as_str())?;
        self.labels.insert(key, value);
        Ok(self)
    }

    /// Add a tag.
    pub fn with_tag(mut self, key: TagKey, value: TagValue) -> crate::Result<Self> {
        check_no_label_conflict(&self.labels, key.as_str())?;
        self.tags.insert(key, value);
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// DerivedParameter.
// ---------------------------------------------------------------------------

/// A parameter whose value is computed from other already-bound values
/// by an [`Expression`].
///
/// Derived parameters may not be used as axes (SRD-0004 D9) — that's
/// enforced by the test-plan layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedParameter {
    /// Parameter name.
    pub name:       ParameterName,
    /// Declared output kind (Integer/Double/Boolean/String).
    pub kind:       ValueKind,
    /// The expression that produces the value.
    pub expression: Expression,
    /// Intrinsic facts.
    #[serde(default)]
    pub labels:     Labels,
    /// Organisational tags.
    #[serde(default)]
    pub tags:       Tags,
}

impl DerivedParameter {
    /// Construct a derived parameter. Rejects `ValueKind::Selection`.
    pub fn new(
        name:       ParameterName,
        kind:       ValueKind,
        expression: Expression,
    ) -> crate::Result<Self> {
        if matches!(kind, ValueKind::Selection) {
            return Err(ParameterError::DerivedSelectionUnsupported.into());
        }
        Ok(Self {
            name,
            kind,
            expression,
            labels: Labels::new(),
            tags: Tags::new(),
        })
    }

    /// Evaluate the expression against the given bindings and wrap the
    /// result in a [`Value`] with `Derived` provenance.
    pub fn compute(&self, bindings: &ValueBindings) -> Result<Value, DerivationError> {
        let raw = self.expression.eval(bindings)?;
        if raw.kind() != self.kind {
            return Err(DerivationError::TypeMismatch {
                op:       format!("derived({})", self.name),
                expected: format!("{:?}", self.kind),
                actual:   format!("{:?}", raw.kind()),
            });
        }
        let generator = Some(GeneratorInfo::Derived {
            expression: format!("{:?}", self.expression),
        });
        Ok(match raw {
            EvalValue::Integer(n) => Value::integer(self.name.clone(), n, generator),
            EvalValue::Double(n) => Value::double(self.name.clone(), n, generator),
            EvalValue::Boolean(b) => Value::boolean(self.name.clone(), b, generator),
            EvalValue::String(s) => Value::string(self.name.clone(), s, generator),
        })
    }

    /// Add a label.
    pub fn with_label(mut self, key: LabelKey, value: LabelValue) -> crate::Result<Self> {
        check_no_tag_conflict(&self.tags, key.as_str())?;
        self.labels.insert(key, value);
        Ok(self)
    }

    /// Add a tag.
    pub fn with_tag(mut self, key: TagKey, value: TagValue) -> crate::Result<Self> {
        check_no_label_conflict(&self.labels, key.as_str())?;
        self.tags.insert(key, value);
        Ok(self)
    }
}

// ---------------------------------------------------------------------------
// Namespace check helpers.
// ---------------------------------------------------------------------------

fn check_no_tag_conflict(tags: &Tags, key: &str) -> Result<(), AttributeError> {
    if tags.keys().any(|k| k.as_str() == key) {
        return Err(AttributeError::DuplicateKey {
            key:   key.to_owned(),
            tiers: vec![Tier::Label, Tier::Tag],
        });
    }
    Ok(())
}

fn check_no_label_conflict(labels: &Labels, key: &str) -> Result<(), AttributeError> {
    if labels.keys().any(|k| k.as_str() == key) {
        return Err(AttributeError::DuplicateKey {
            key:   key.to_owned(),
            tiers: vec![Tier::Label, Tier::Tag],
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Outer Parameter enum.
// ---------------------------------------------------------------------------

/// Kind-tagged parameter.
///
/// See per-variant structs for constructors and builder-style setters.
/// Dispatch methods (`name`, `kind`, `default`, `generate`, …) forward
/// to the inner variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Parameter {
    /// Integer parameter.
    Integer(IntegerParameter),
    /// Double parameter.
    Double(DoubleParameter),
    /// Boolean parameter.
    Boolean(BooleanParameter),
    /// String parameter.
    String(StringParameter),
    /// Selection parameter.
    Selection(SelectionParameter),
    /// Derived parameter.
    Derived(DerivedParameter),
}

impl Parameter {
    /// Parameter name.
    #[must_use]
    pub const fn name(&self) -> &ParameterName {
        match self {
            Self::Integer(p) => &p.name,
            Self::Double(p) => &p.name,
            Self::Boolean(p) => &p.name,
            Self::String(p) => &p.name,
            Self::Selection(p) => &p.name,
            Self::Derived(p) => &p.name,
        }
    }

    /// Parameter kind.
    #[must_use]
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Integer(_) => ValueKind::Integer,
            Self::Double(_) => ValueKind::Double,
            Self::Boolean(_) => ValueKind::Boolean,
            Self::String(_) => ValueKind::String,
            Self::Selection(_) => ValueKind::Selection,
            Self::Derived(p) => p.kind,
        }
    }

    /// Borrowed domain view. Returns `None` for [`Self::Derived`]
    /// (derived parameters don't own a domain in this SRD tier).
    #[must_use]
    pub const fn domain(&self) -> Option<Domain<'_>> {
        Some(match self {
            Self::Integer(p) => Domain::Integer {
                parameter: &p.name,
                domain:    &p.domain,
            },
            Self::Double(p) => Domain::Double {
                parameter: &p.name,
                domain:    &p.domain,
            },
            Self::Boolean(p) => Domain::Boolean { parameter: &p.name },
            Self::String(p) => Domain::String {
                parameter: &p.name,
                domain:    &p.domain,
            },
            Self::Selection(p) => Domain::Selection {
                parameter: &p.name,
                domain:    &p.domain,
            },
            Self::Derived(_) => return None,
        })
    }

    /// The default value, wrapped in [`Value`] with `Default` provenance,
    /// if one is set.
    #[must_use]
    pub fn default(&self) -> Option<Value> {
        let generator = Some(GeneratorInfo::Default);
        match self {
            Self::Integer(p) => p
                .default
                .map(|d| Value::integer(p.name.clone(), d, generator)),
            Self::Double(p) => p
                .default
                .map(|d| Value::double(p.name.clone(), d, generator)),
            Self::Boolean(p) => p
                .default
                .map(|d| Value::boolean(p.name.clone(), d, generator)),
            Self::String(p) => p
                .default
                .clone()
                .map(|d| Value::string(p.name.clone(), d, generator)),
            Self::Selection(p) => p
                .default
                .clone()
                .map(|d| Value::selection(p.name.clone(), d, generator)),
            Self::Derived(_) => None,
        }
    }

    /// Pick a value. Uses the registered default if present, else
    /// samples uniformly from the domain.
    ///
    /// # Panics
    ///
    /// Panics when called on [`Self::Derived`] (derived parameters are
    /// computed from bindings, not sampled) or on domains whose
    /// sampling is not implemented (regex, external selection).
    pub fn generate<R: Rng + ?Sized>(&self, rng: &mut R) -> Value {
        if let Some(d) = self.default() {
            return d;
        }
        self.generate_random(rng)
    }

    /// Always draws from the domain, ignoring any default.
    ///
    /// # Panics
    ///
    /// Panics on [`Self::Derived`] and on domains whose sampling is
    /// unimplemented.
    pub fn generate_random<R: Rng + ?Sized>(&self, rng: &mut R) -> Value {
        if let Self::Derived(_) = self {
            unimplemented!(
                "derived parameters do not support direct sampling; use DerivedParameter::compute"
            );
        }
        let domain = self.domain().expect("non-derived has a domain");
        domain.sample(rng)
    }

    /// Pick a boundary value from the domain.
    ///
    /// # Panics
    ///
    /// Panics on [`Self::Derived`] (no domain).
    pub fn generate_boundary<R: Rng + ?Sized>(&self, rng: &mut R) -> Value {
        let domain = self
            .domain()
            .expect("generate_boundary is undefined for derived parameters");
        let boundaries = domain.boundary_values();
        if boundaries.is_empty() {
            return domain.sample(rng);
        }
        let idx = rng.gen_range(0..boundaries.len());
        boundaries.into_iter().nth(idx).expect("idx < len")
    }

    /// Validate a candidate [`Value`] against this parameter's kind,
    /// domain, and registered constraints.
    #[must_use]
    pub fn validate(&self, value: &Value) -> ValidationResult {
        if value.kind() != self.kind() {
            return ValidationResult::failed(
                "kind mismatch",
                vec![format!(
                    "expected {:?}, got {:?}",
                    self.kind(),
                    value.kind()
                )],
            );
        }

        let mut violations = Vec::new();
        match (self, value) {
            (Self::Integer(p), Value::Integer(v)) => {
                if !p.domain.contains_native(v.value) {
                    violations.push(format!("value {} not in domain", v.value));
                }
                for c in &p.constraints {
                    if !c.test(v.value) {
                        violations.push("constraint not satisfied".to_owned());
                    }
                }
            }
            (Self::Double(p), Value::Double(v)) => {
                if !p.domain.contains_native(v.value) {
                    violations.push(format!("value {} not in domain", v.value));
                }
                for c in &p.constraints {
                    if !c.test(v.value) {
                        violations.push("constraint not satisfied".to_owned());
                    }
                }
            }
            (Self::Boolean(p), Value::Boolean(v)) => {
                for c in &p.constraints {
                    if !c.test(v.value) {
                        violations.push("constraint not satisfied".to_owned());
                    }
                }
            }
            (Self::String(p), Value::String(v)) => {
                if !p.domain.contains_native(&v.value) {
                    violations.push("value not in domain".to_owned());
                }
                for c in &p.constraints {
                    if !c.test(&v.value) {
                        violations.push("constraint not satisfied".to_owned());
                    }
                }
            }
            (Self::Selection(p), Value::Selection(v)) => {
                // Only fixed domains can be shape-checked without a
                // resolver; external selections accept anything of the
                // right shape here.
                if matches!(p.domain, SelectionDomain::Fixed { .. })
                    && !p.domain.contains_items_fixed(&v.items)
                {
                    violations.push("selection not in domain".to_owned());
                }
                for c in &p.constraints {
                    if !c.test(&v.items) {
                        violations.push("constraint not satisfied".to_owned());
                    }
                }
            }
            (Self::Derived(_), _) => {
                // Derived parameters have no domain to check at this
                // layer — their validity is the expression's validity.
            }
            _ => unreachable!("kind match enforced above"),
        }

        if violations.is_empty() {
            ValidationResult::Passed
        } else {
            ValidationResult::failed("validation failed", violations)
        }
    }

    /// Best-effort "is any value in the domain satisfied by this
    /// constraint?" check. Tests the domain's boundary values plus up
    /// to eight deterministic random samples (seeded for
    /// reproducibility). False negatives possible; false positives are
    /// not.
    ///
    /// Returns `false` for derived parameters (no domain to probe),
    /// for kind mismatches, and for domains that panic on sampling
    /// (regex, external selection) — those would need a richer check.
    #[must_use]
    pub fn satisfies(&self, c: &Constraint) -> bool {
        if c.kind() != self.kind() {
            return false;
        }
        match (self, c) {
            (Self::Integer(p), Constraint::Integer(ic)) => {
                for b in p.domain.boundaries_native() {
                    if ic.test(b) {
                        return true;
                    }
                }
                let mut rng = StdRng::seed_from_u64(SATISFIES_SEED);
                for _ in 0..SATISFIES_SAMPLES {
                    if ic.test(p.domain.sample_native(&mut rng)) {
                        return true;
                    }
                }
                false
            }
            (Self::Double(p), Constraint::Double(dc)) => {
                for b in p.domain.boundaries_native() {
                    if dc.test(b) {
                        return true;
                    }
                }
                let mut rng = StdRng::seed_from_u64(SATISFIES_SEED);
                for _ in 0..SATISFIES_SAMPLES {
                    if dc.test(p.domain.sample_native(&mut rng)) {
                        return true;
                    }
                }
                false
            }
            (Self::Boolean(_), Constraint::Boolean(bc)) => bc.test(true) || bc.test(false),
            (Self::String(_), Constraint::String(sc)) => {
                // Only probe boundaries — sampling "Any" is empty and
                // regex domain sampling is a documented gap.
                sc.test("")
            }
            (Self::Selection(p), Constraint::Selection(sc)) => {
                // Boundaries only; external domains return no boundaries
                // without a resolver.
                for boundary in p.domain.boundaries_fixed() {
                    let iset: IndexSet<SelectionItem> = boundary.into_iter().collect();
                    if sc.test(&iset) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }
}

// Convenience helpers on Constraint for the dispatcher above.
impl Constraint {
    const fn kind(&self) -> ValueKind {
        match self {
            Self::Integer(_) => ValueKind::Integer,
            Self::Double(_) => ValueKind::Double,
            Self::Boolean(_) => ValueKind::Boolean,
            Self::String(_) => ValueKind::String,
            Self::Selection(_) => ValueKind::Selection,
        }
    }
}

const SATISFIES_SEED: u64 = 0x5a71_5f1e_55e5_d007;
const SATISFIES_SAMPLES: u32 = 8;

// ---------------------------------------------------------------------------
// Attributed impls.
// ---------------------------------------------------------------------------

impl Attributed for Parameter {
    fn labels(&self) -> &Labels {
        match self {
            Self::Integer(p) => &p.labels,
            Self::Double(p) => &p.labels,
            Self::Boolean(p) => &p.labels,
            Self::String(p) => &p.labels,
            Self::Selection(p) => &p.labels,
            Self::Derived(p) => &p.labels,
        }
    }

    fn tags(&self) -> &Tags {
        match self {
            Self::Integer(p) => &p.tags,
            Self::Double(p) => &p.tags,
            Self::Boolean(p) => &p.tags,
            Self::String(p) => &p.tags,
            Self::Selection(p) => &p.tags,
            Self::Derived(p) => &p.tags,
        }
    }
}

impl Attributed for IntegerParameter {
    fn labels(&self) -> &Labels {
        &self.labels
    }
    fn tags(&self) -> &Tags {
        &self.tags
    }
}

impl Attributed for DoubleParameter {
    fn labels(&self) -> &Labels {
        &self.labels
    }
    fn tags(&self) -> &Tags {
        &self.tags
    }
}

impl Attributed for BooleanParameter {
    fn labels(&self) -> &Labels {
        &self.labels
    }
    fn tags(&self) -> &Tags {
        &self.tags
    }
}

impl Attributed for StringParameter {
    fn labels(&self) -> &Labels {
        &self.labels
    }
    fn tags(&self) -> &Tags {
        &self.tags
    }
}

impl Attributed for SelectionParameter {
    fn labels(&self) -> &Labels {
        &self.labels
    }
    fn tags(&self) -> &Tags {
        &self.tags
    }
}

impl Attributed for DerivedParameter {
    fn labels(&self) -> &Labels {
        &self.labels
    }
    fn tags(&self) -> &Tags {
        &self.tags
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;

    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }

    fn rng() -> StdRng {
        StdRng::seed_from_u64(7)
    }

    // ---------- IntegerParameter ----------

    #[test]
    fn integer_range_constructor() {
        let p = IntegerParameter::range(pname("n"), 1, 10).unwrap();
        assert_eq!(p.name.as_str(), "n");
        assert_eq!(p.default, None);
    }

    #[test]
    fn integer_with_default_and_constraint() {
        let p = IntegerParameter::range(pname("n"), 1, 10)
            .unwrap()
            .with_constraint(IntConstraint::Min { n: 3 })
            .with_default(5)
            .unwrap();
        assert_eq!(p.default, Some(5));

        // Default outside domain is rejected.
        let err = IntegerParameter::range(pname("n"), 1, 10)
            .unwrap()
            .with_default(42)
            .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Parameter(ParameterError::DefaultNotInDomain)
        ));

        // Default violating a constraint is rejected.
        let err = IntegerParameter::range(pname("n"), 1, 10)
            .unwrap()
            .with_constraint(IntConstraint::Min { n: 5 })
            .with_default(3)
            .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Parameter(ParameterError::DefaultViolatesConstraint)
        ));
    }

    #[test]
    fn integer_label_tag_namespace_enforcement() {
        let p = IntegerParameter::range(pname("n"), 1, 10).unwrap();
        let p = p
            .with_label(LabelKey::new("type").unwrap(), LabelValue::new("threads").unwrap())
            .unwrap();
        // Adding a tag with the same key is rejected. `?` converts
        // AttributeError straight through crate::Error::Attribute, so
        // we match that variant directly.
        let err = p
            .with_tag(TagKey::new("type").unwrap(), TagValue::new("bench").unwrap())
            .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Attribute(AttributeError::DuplicateKey { .. })
        ));
    }

    // ---------- DoubleParameter ----------

    #[test]
    fn double_parameter_roundtrip() {
        let p = DoubleParameter::range(pname("r"), 0.0, 1.0)
            .unwrap()
            .with_default(0.5)
            .unwrap();
        assert_eq!(p.default, Some(0.5));
    }

    // ---------- BooleanParameter ----------

    #[test]
    fn boolean_parameter_with_default_and_constraint() {
        let p = BooleanParameter::of(pname("flag"))
            .with_constraint(BoolConstraint::EqTo { b: true })
            .with_default(true)
            .unwrap();
        assert_eq!(p.default, Some(true));

        let err = BooleanParameter::of(pname("flag"))
            .with_constraint(BoolConstraint::EqTo { b: true })
            .with_default(false)
            .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Parameter(ParameterError::DefaultViolatesConstraint)
        ));
    }

    // ---------- StringParameter ----------

    #[test]
    fn string_regex_parameter_rejects_non_matching_default() {
        let err = StringParameter::regex(pname("s"), "^foo$")
            .unwrap()
            .with_default("bar")
            .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Parameter(ParameterError::DefaultNotInDomain)
        ));
    }

    // ---------- SelectionParameter ----------

    #[test]
    fn selection_parameter_default_subset_check() {
        let values: IndexSet<SelectionItem> =
            ["a", "b", "c"].iter().map(|s| SelectionItem::new(*s).unwrap()).collect();
        let p = SelectionParameter::of(pname("s"), values, 2).unwrap();

        let good: IndexSet<SelectionItem> =
            std::iter::once(SelectionItem::new("a").unwrap()).collect();
        assert!(p.clone().with_default(good).is_ok());

        let bad: IndexSet<SelectionItem> =
            std::iter::once(SelectionItem::new("z").unwrap()).collect();
        let err = p.with_default(bad).unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Parameter(ParameterError::DefaultNotInDomain)
        ));
    }

    // ---------- Outer Parameter ----------

    #[test]
    fn parameter_name_kind_and_domain_dispatch() {
        let p: Parameter = Parameter::Integer(
            IntegerParameter::range(pname("n"), 1, 10).unwrap(),
        );
        assert_eq!(p.name().as_str(), "n");
        assert_eq!(p.kind(), ValueKind::Integer);
        assert!(p.domain().is_some());
    }

    #[test]
    fn parameter_generate_prefers_default() {
        let p = Parameter::Integer(
            IntegerParameter::range(pname("n"), 1, 10)
                .unwrap()
                .with_default(7)
                .unwrap(),
        );
        let mut r = rng();
        let v = p.generate(&mut r);
        assert_eq!(v.as_integer(), Some(7));
        // Default provenance.
        match v.provenance().generator.as_ref().unwrap() {
            GeneratorInfo::Default => {}
            other => panic!("expected Default, got {other:?}"),
        }
    }

    #[test]
    fn parameter_generate_random_draws_from_domain() {
        let p = Parameter::Integer(
            IntegerParameter::range(pname("n"), 1, 10).unwrap(),
        );
        let mut r = rng();
        for _ in 0..20 {
            let v = p.generate_random(&mut r);
            let n = v.as_integer().unwrap();
            assert!((1..=10).contains(&n));
        }
    }

    #[test]
    fn parameter_generate_boundary_hits_an_endpoint() {
        let p = Parameter::Integer(
            IntegerParameter::range(pname("n"), 1, 10).unwrap(),
        );
        let mut r = rng();
        let mut seen = BTreeSet::new();
        for _ in 0..50 {
            let v = p.generate_boundary(&mut r);
            seen.insert(v.as_integer().unwrap());
        }
        assert!(seen.contains(&1) || seen.contains(&10));
    }

    #[test]
    fn parameter_validate_catches_kind_and_domain() {
        let p = Parameter::Integer(
            IntegerParameter::range(pname("n"), 1, 10).unwrap(),
        );
        let ok = Value::integer(pname("n"), 5, None);
        assert!(p.validate(&ok).is_passed());

        let out_of_range = Value::integer(pname("n"), 42, None);
        assert!(p.validate(&out_of_range).is_failed());

        let wrong_kind = Value::boolean(pname("n"), true, None);
        assert!(p.validate(&wrong_kind).is_failed());
    }

    #[test]
    fn parameter_satisfies_hits_constraint_via_boundaries() {
        let p = Parameter::Integer(
            IntegerParameter::range(pname("n"), 1, 10).unwrap(),
        );
        assert!(p.satisfies(&Constraint::Integer(IntConstraint::Min { n: 5 })));
        assert!(!p.satisfies(&Constraint::Integer(IntConstraint::Min { n: 100 })));
        // Kind mismatch yields false.
        assert!(!p.satisfies(&Constraint::Boolean(BoolConstraint::EqTo { b: true })));
    }

    #[test]
    fn parameter_attributed_trait() {
        let p = Parameter::Integer(
            IntegerParameter::range(pname("n"), 1, 10).unwrap(),
        );
        assert!(<Parameter as Attributed>::labels(&p).is_empty());
        assert!(<Parameter as Attributed>::tags(&p).is_empty());
    }

    #[test]
    fn parameter_serde_roundtrip() {
        let p = Parameter::Integer(
            IntegerParameter::range(pname("n"), 1, 10)
                .unwrap()
                .with_default(5)
                .unwrap(),
        );
        let json = serde_json::to_string(&p).unwrap();
        let back: Parameter = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }

    // ---------- DerivedParameter ----------

    #[test]
    fn derived_parameter_computes_from_bindings() {
        use crate::expression::{BinOp, Expression, Literal};
        let expr = Expression::binop(
            BinOp::Mul,
            Expression::reference(pname("threads")),
            Expression::literal(Literal::Integer { value: 2 }),
        );
        let p = DerivedParameter::new(pname("double_threads"), ValueKind::Integer, expr).unwrap();

        let mut bindings = ValueBindings::new();
        bindings.insert(pname("threads"), Value::integer(pname("threads"), 8, None));
        let out = p.compute(&bindings).unwrap();
        assert_eq!(out.as_integer(), Some(16));
    }

    #[test]
    fn derived_parameter_rejects_selection_kind() {
        use crate::expression::{Expression, Literal};
        let err = DerivedParameter::new(
            pname("bad"),
            ValueKind::Selection,
            Expression::literal(Literal::Integer { value: 1 }),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            crate::Error::Parameter(ParameterError::DerivedSelectionUnsupported)
        ));
    }

    #[test]
    fn derived_parameter_kind_mismatch_errors() {
        use crate::expression::{Expression, Literal};
        // Declared as Double but expression yields Integer.
        let p = DerivedParameter::new(
            pname("d"),
            ValueKind::Double,
            Expression::literal(Literal::Integer { value: 1 }),
        )
        .unwrap();
        let err = p.compute(&ValueBindings::new()).unwrap_err();
        assert!(matches!(err, DerivationError::TypeMismatch { .. }));
    }

    #[test]
    fn outer_parameter_with_derived_variant() {
        use crate::expression::{Expression, Literal};
        let p = Parameter::Derived(
            DerivedParameter::new(
                pname("c"),
                ValueKind::Integer,
                Expression::literal(Literal::Integer { value: 3 }),
            )
            .unwrap(),
        );
        assert_eq!(p.kind(), ValueKind::Integer);
        // Derived has no domain.
        assert!(p.domain().is_none());
    }
}
