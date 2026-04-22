// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! Expression language for [`DerivedParameter`](crate::parameter::DerivedParameter).
//!
//! A declarative AST — literals, parameter references, binary/unary
//! ops, built-in functions, and conditionals — serde-able end to end so
//! plan files fingerprint deterministically. Evaluated against a
//! [`ValueBindings`] map (the already-bound parameter values).
//!
//! Type checking happens at [`Expression::eval`]: an expression is only
//! statically checked for shape at construction (the AST type system
//! does that); kind errors surface at bind time with
//! [`DerivationError::TypeMismatch`]. This matches the upstream Java
//! behaviour where `compute()` throws at runtime.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::names::ParameterName;
use crate::value::{Value, ValueKind};

/// Parameter-name → value lookup passed to [`Expression::eval`].
///
/// Aliased rather than newtyped so callers can build bindings however
/// is convenient — from iterators, from a hand-constructed `HashMap`,
/// whatever. A later slice can upgrade this to a trait once the plan-
/// binder lands.
pub type ValueBindings = HashMap<ParameterName, Value>;

// ---------------------------------------------------------------------------
// DerivationError.
// ---------------------------------------------------------------------------

/// Errors produced at [`Expression::eval`] time.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DerivationError {
    /// A `Ref(name)` pointed at a parameter that isn't in the bindings.
    #[error("unknown parameter reference: {0}")]
    UnknownParameter(ParameterName),

    /// An operation received operands of the wrong kind.
    #[error("type mismatch in {op}: expected {expected}, got {actual}")]
    TypeMismatch {
        /// Operator or builtin name.
        op:       String,
        /// The kind the operator expected.
        expected: String,
        /// The kind that was actually supplied.
        actual:   String,
    },

    /// Integer division or modulo by zero.
    #[error("division by zero")]
    DivisionByZero,

    /// A builtin received the wrong number of arguments.
    #[error("{builtin} expects {expected} argument(s), got {actual}")]
    InvalidArity {
        /// Builtin name.
        builtin:  String,
        /// Expected arity.
        expected: usize,
        /// Actual arity.
        actual:   usize,
    },

    /// Selection values can't flow through arithmetic/logic expressions.
    #[error("selection values are not supported in derivation expressions")]
    SelectionNotSupported,
}

// ---------------------------------------------------------------------------
// EvalValue — the typed result of evaluating an Expression.
// ---------------------------------------------------------------------------

/// Typed value produced by [`Expression::eval`].
///
/// Distinct from [`Value`] because an intermediate expression result
/// has no parameter name — it's a raw typed value that the calling
/// derived parameter then wraps with its own name and provenance.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalValue {
    /// 64-bit signed integer.
    Integer(i64),
    /// IEEE-754 `f64`.
    Double(f64),
    /// Boolean.
    Boolean(bool),
    /// UTF-8 string.
    String(String),
}

impl EvalValue {
    /// Discriminator.
    #[must_use]
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Integer(_) => ValueKind::Integer,
            Self::Double(_) => ValueKind::Double,
            Self::Boolean(_) => ValueKind::Boolean,
            Self::String(_) => ValueKind::String,
        }
    }

    /// `i64` accessor.
    #[must_use]
    pub const fn as_integer(&self) -> Option<i64> {
        if let Self::Integer(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    /// `f64` accessor.
    #[must_use]
    pub const fn as_double(&self) -> Option<f64> {
        if let Self::Double(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    /// `bool` accessor.
    #[must_use]
    pub const fn as_boolean(&self) -> Option<bool> {
        if let Self::Boolean(v) = self {
            Some(*v)
        } else {
            None
        }
    }

    /// `str` accessor.
    #[must_use]
    pub fn as_string(&self) -> Option<&str> {
        if let Self::String(v) = self {
            Some(v)
        } else {
            None
        }
    }

    const fn kind_label(&self) -> &'static str {
        match self {
            Self::Integer(_) => "integer",
            Self::Double(_) => "double",
            Self::Boolean(_) => "boolean",
            Self::String(_) => "string",
        }
    }
}

impl TryFrom<&Value> for EvalValue {
    type Error = DerivationError;
    fn try_from(v: &Value) -> Result<Self, Self::Error> {
        Ok(match v {
            Value::Integer(i) => Self::Integer(i.value),
            Value::Double(d) => Self::Double(d.value),
            Value::Boolean(b) => Self::Boolean(b.value),
            Value::String(s) => Self::String(s.value.clone()),
            Value::Selection(_) => return Err(DerivationError::SelectionNotSupported),
        })
    }
}

// ---------------------------------------------------------------------------
// Expression AST.
// ---------------------------------------------------------------------------

/// A literal value embedded in an expression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Literal {
    /// `i64` literal.
    Integer {
        /// The integer.
        value: i64,
    },
    /// `f64` literal. `NaN` is not forbidden here but arithmetic will
    /// propagate it per IEEE-754.
    Double {
        /// The float.
        value: f64,
    },
    /// Boolean literal.
    Boolean {
        /// The flag.
        value: bool,
    },
    /// String literal.
    String {
        /// The text.
        value: String,
    },
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinOp {
    /// Arithmetic addition. Numeric operands only.
    Add,
    /// Arithmetic subtraction.
    Sub,
    /// Arithmetic multiplication.
    Mul,
    /// Arithmetic division. Integer division rejects zero divisors.
    Div,
    /// Arithmetic modulo. Integer only; rejects zero divisors.
    Mod,
    /// Equality. Same-kind operands.
    Eq,
    /// Inequality.
    Ne,
    /// Less than. Numeric.
    Lt,
    /// Less than or equal. Numeric.
    Le,
    /// Greater than. Numeric.
    Gt,
    /// Greater than or equal. Numeric.
    Ge,
    /// Logical AND. Boolean operands.
    And,
    /// Logical OR. Boolean operands.
    Or,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnOp {
    /// Numeric negation.
    Neg,
    /// Boolean negation.
    Not,
}

/// Built-in functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinFn {
    /// `ceil(f64) -> i64`.
    Ceil,
    /// `floor(f64) -> i64`.
    Floor,
    /// `round(f64) -> i64` (ties-to-even per IEEE).
    Round,
    /// `min(numeric, numeric, ...) -> numeric` (same kind).
    Min,
    /// `max(numeric, numeric, ...) -> numeric`.
    Max,
    /// `abs(numeric) -> numeric`.
    Abs,
    /// `pow(f64, f64) -> f64`.
    Pow,
    /// `len(string) -> i64` (byte length).
    Len,
}

impl BuiltinFn {
    const fn label(self) -> &'static str {
        match self {
            Self::Ceil => "ceil",
            Self::Floor => "floor",
            Self::Round => "round",
            Self::Min => "min",
            Self::Max => "max",
            Self::Abs => "abs",
            Self::Pow => "pow",
            Self::Len => "len",
        }
    }
}

/// An expression tree. Serde-able end to end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "node", rename_all = "snake_case")]
pub enum Expression {
    /// Literal value.
    Literal {
        /// The literal.
        value: Literal,
    },
    /// Reference to a previously-bound parameter value.
    Ref {
        /// The parameter name.
        name: ParameterName,
    },
    /// Binary operation.
    BinOp {
        /// Operator.
        op:  BinOp,
        /// Left operand.
        lhs: Box<Self>,
        /// Right operand.
        rhs: Box<Self>,
    },
    /// Unary operation.
    UnOp {
        /// Operator.
        op:  UnOp,
        /// Operand.
        arg: Box<Self>,
    },
    /// Built-in function call.
    Call {
        /// Builtin.
        func: BuiltinFn,
        /// Arguments.
        args: Vec<Self>,
    },
    /// Conditional (ternary).
    If {
        /// Boolean condition.
        cond:  Box<Self>,
        /// Value when `cond` is true.
        then_: Box<Self>,
        /// Value when `cond` is false.
        else_: Box<Self>,
    },
}

impl Expression {
    /// Convenience constructor for a literal.
    #[must_use]
    pub const fn literal(lit: Literal) -> Self {
        Self::Literal { value: lit }
    }

    /// Convenience constructor for a parameter reference.
    #[must_use]
    pub const fn reference(name: ParameterName) -> Self {
        Self::Ref { name }
    }

    /// Convenience constructor for a binary operation.
    #[must_use]
    pub fn binop(op: BinOp, lhs: Self, rhs: Self) -> Self {
        Self::BinOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// Convenience constructor for a unary operation.
    #[must_use]
    pub fn unop(op: UnOp, arg: Self) -> Self {
        Self::UnOp {
            op,
            arg: Box::new(arg),
        }
    }

    /// Convenience constructor for a builtin call.
    #[must_use]
    pub const fn call(func: BuiltinFn, args: Vec<Self>) -> Self {
        Self::Call { func, args }
    }

    /// Convenience constructor for an if-expression.
    #[must_use]
    pub fn if_then_else(cond: Self, then_: Self, else_: Self) -> Self {
        Self::If {
            cond:  Box::new(cond),
            then_: Box::new(then_),
            else_: Box::new(else_),
        }
    }

    /// Evaluate this expression against the given bindings.
    pub fn eval(&self, bindings: &ValueBindings) -> Result<EvalValue, DerivationError> {
        match self {
            Self::Literal { value } => Ok(eval_literal(value)),
            Self::Ref { name } => bindings.get(name).map_or_else(
                || Err(DerivationError::UnknownParameter(name.clone())),
                EvalValue::try_from,
            ),
            Self::BinOp { op, lhs, rhs } => {
                let l = lhs.eval(bindings)?;
                let r = rhs.eval(bindings)?;
                eval_binop(*op, l, r)
            }
            Self::UnOp { op, arg } => {
                let a = arg.eval(bindings)?;
                eval_unop(*op, a)
            }
            Self::Call { func, args } => {
                let vs: Result<Vec<EvalValue>, _> =
                    args.iter().map(|a| a.eval(bindings)).collect();
                eval_call(*func, vs?)
            }
            Self::If { cond, then_, else_ } => {
                let c = cond.eval(bindings)?;
                match c {
                    EvalValue::Boolean(true) => then_.eval(bindings),
                    EvalValue::Boolean(false) => else_.eval(bindings),
                    other => Err(type_mismatch("if", "boolean", &other)),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Evaluation helpers.
// ---------------------------------------------------------------------------

fn eval_literal(lit: &Literal) -> EvalValue {
    match lit {
        Literal::Integer { value } => EvalValue::Integer(*value),
        Literal::Double { value } => EvalValue::Double(*value),
        Literal::Boolean { value } => EvalValue::Boolean(*value),
        Literal::String { value } => EvalValue::String(value.clone()),
    }
}

fn type_mismatch(op: &str, expected: &str, actual: &EvalValue) -> DerivationError {
    DerivationError::TypeMismatch {
        op:       op.to_owned(),
        expected: expected.to_owned(),
        actual:   actual.kind_label().to_owned(),
    }
}

fn eval_binop(op: BinOp, l: EvalValue, r: EvalValue) -> Result<EvalValue, DerivationError> {
    use EvalValue as E;
    let op_label = binop_label(op);
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul => match (l, r) {
            (E::Integer(a), E::Integer(b)) => Ok(E::Integer(match op {
                BinOp::Add => a.wrapping_add(b),
                BinOp::Sub => a.wrapping_sub(b),
                BinOp::Mul => a.wrapping_mul(b),
                _ => unreachable!(),
            })),
            (E::Double(a), E::Double(b)) => Ok(E::Double(match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                _ => unreachable!(),
            })),
            (a, _) => Err(type_mismatch(op_label, "matching numeric operands", &a)),
        },
        BinOp::Div => match (l, r) {
            (E::Integer(_), E::Integer(0)) => Err(DerivationError::DivisionByZero),
            (E::Integer(a), E::Integer(b)) => Ok(E::Integer(a / b)),
            (E::Double(a), E::Double(b)) => Ok(E::Double(a / b)),
            (a, _) => Err(type_mismatch(op_label, "matching numeric operands", &a)),
        },
        BinOp::Mod => match (l, r) {
            (E::Integer(_), E::Integer(0)) => Err(DerivationError::DivisionByZero),
            (E::Integer(a), E::Integer(b)) => Ok(E::Integer(a % b)),
            (a, _) => Err(type_mismatch(op_label, "integer operands", &a)),
        },
        BinOp::Eq => Ok(E::Boolean(values_equal(&l, &r)?)),
        BinOp::Ne => Ok(E::Boolean(!values_equal(&l, &r)?)),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => compare_numeric(op, &l, &r),
        BinOp::And => match (l, r) {
            (E::Boolean(a), E::Boolean(b)) => Ok(E::Boolean(a && b)),
            (a, _) => Err(type_mismatch(op_label, "boolean operands", &a)),
        },
        BinOp::Or => match (l, r) {
            (E::Boolean(a), E::Boolean(b)) => Ok(E::Boolean(a || b)),
            (a, _) => Err(type_mismatch(op_label, "boolean operands", &a)),
        },
    }
}

const fn binop_label(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

fn values_equal(l: &EvalValue, r: &EvalValue) -> Result<bool, DerivationError> {
    use EvalValue as E;
    Ok(match (l, r) {
        (E::Integer(a), E::Integer(b)) => a == b,
        #[allow(
            clippy::float_cmp,
            reason = "IEEE equality is the intended semantics here"
        )]
        (E::Double(a), E::Double(b)) => a == b,
        (E::Boolean(a), E::Boolean(b)) => a == b,
        (E::String(a), E::String(b)) => a == b,
        (a, _) => return Err(type_mismatch("eq", "matching operands", a)),
    })
}

fn compare_numeric(
    op: BinOp,
    l:  &EvalValue,
    r:  &EvalValue,
) -> Result<EvalValue, DerivationError> {
    use std::cmp::Ordering;

    use EvalValue as E;
    let op_label = binop_label(op);
    let ord = match (l, r) {
        (E::Integer(a), E::Integer(b)) => a.cmp(b),
        (E::Double(a), E::Double(b)) => a.total_cmp(b),
        (a, _) => return Err(type_mismatch(op_label, "matching numeric operands", a)),
    };
    Ok(EvalValue::Boolean(match op {
        BinOp::Lt => ord == Ordering::Less,
        BinOp::Le => ord != Ordering::Greater,
        BinOp::Gt => ord == Ordering::Greater,
        BinOp::Ge => ord != Ordering::Less,
        _ => unreachable!(),
    }))
}

fn eval_unop(op: UnOp, a: EvalValue) -> Result<EvalValue, DerivationError> {
    use EvalValue as E;
    match op {
        UnOp::Neg => match a {
            E::Integer(n) => Ok(E::Integer(n.wrapping_neg())),
            E::Double(n) => Ok(E::Double(-n)),
            other => Err(type_mismatch("neg", "numeric operand", &other)),
        },
        UnOp::Not => match a {
            E::Boolean(b) => Ok(E::Boolean(!b)),
            other => Err(type_mismatch("not", "boolean operand", &other)),
        },
    }
}

fn eval_call(func: BuiltinFn, args: Vec<EvalValue>) -> Result<EvalValue, DerivationError> {
    use EvalValue as E;
    match func {
        BuiltinFn::Ceil | BuiltinFn::Floor | BuiltinFn::Round => {
            check_arity(func, &args, 1)?;
            match &args[0] {
                E::Double(v) => {
                    let folded = match func {
                        BuiltinFn::Ceil => v.ceil(),
                        BuiltinFn::Floor => v.floor(),
                        BuiltinFn::Round => v.round_ties_even(),
                        _ => unreachable!(),
                    };
                    // Clamp to i64 range so we can return Integer. The
                    // precision/truncation concerns are by construction:
                    // any Integer-shaped output from ceil/floor/round of
                    // an f64 rounds through the i64 range.
                    #[allow(
                        clippy::cast_precision_loss,
                        clippy::cast_possible_truncation,
                        reason = "deliberate i64↔f64 projection for ceil/floor/round"
                    )]
                    let clamped = folded.clamp(i64::MIN as f64, i64::MAX as f64) as i64;
                    Ok(E::Integer(clamped))
                }
                other => Err(type_mismatch(func.label(), "double", other)),
            }
        }
        BuiltinFn::Min | BuiltinFn::Max => {
            if args.len() < 2 {
                return Err(DerivationError::InvalidArity {
                    builtin:  func.label().to_owned(),
                    expected: 2,
                    actual:   args.len(),
                });
            }
            fold_minmax(func, args)
        }
        BuiltinFn::Abs => {
            check_arity(func, &args, 1)?;
            match &args[0] {
                E::Integer(n) => Ok(E::Integer(n.wrapping_abs())),
                E::Double(n) => Ok(E::Double(n.abs())),
                other => Err(type_mismatch(func.label(), "numeric", other)),
            }
        }
        BuiltinFn::Pow => {
            check_arity(func, &args, 2)?;
            match (&args[0], &args[1]) {
                (E::Double(b), E::Double(e)) => Ok(E::Double(b.powf(*e))),
                (a, _) => Err(type_mismatch(func.label(), "double base", a)),
            }
        }
        BuiltinFn::Len => {
            check_arity(func, &args, 1)?;
            match &args[0] {
                E::String(s) => {
                    let len = i64::try_from(s.len()).expect("string length fits in i64");
                    Ok(E::Integer(len))
                }
                other => Err(type_mismatch(func.label(), "string", other)),
            }
        }
    }
}

fn check_arity(
    func:     BuiltinFn,
    args:     &[EvalValue],
    expected: usize,
) -> Result<(), DerivationError> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(DerivationError::InvalidArity {
            builtin: func.label().to_owned(),
            expected,
            actual:  args.len(),
        })
    }
}

fn fold_minmax(
    func: BuiltinFn,
    args: Vec<EvalValue>,
) -> Result<EvalValue, DerivationError> {
    use EvalValue as E;
    let mut iter = args.into_iter();
    let first = iter.next().expect("≥2 args verified above");
    iter.try_fold(first, |acc, next| match (acc, next) {
        (E::Integer(a), E::Integer(b)) => Ok(E::Integer(if func == BuiltinFn::Min {
            a.min(b)
        } else {
            a.max(b)
        })),
        (E::Double(a), E::Double(b)) => Ok(E::Double(if func == BuiltinFn::Min {
            a.min(b)
        } else {
            a.max(b)
        })),
        (a, _) => Err(type_mismatch(func.label(), "matching numeric arguments", &a)),
    })
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pname(s: &str) -> ParameterName {
        ParameterName::new(s).unwrap()
    }

    fn empty_bindings() -> ValueBindings {
        ValueBindings::new()
    }

    // ---------- Literal / Ref ----------

    #[test]
    fn literal_evaluates_to_eval_value() {
        let b = empty_bindings();
        assert_eq!(
            Expression::literal(Literal::Integer { value: 7 }).eval(&b).unwrap(),
            EvalValue::Integer(7)
        );
        assert_eq!(
            Expression::literal(Literal::Boolean { value: true }).eval(&b).unwrap(),
            EvalValue::Boolean(true)
        );
    }

    #[test]
    fn ref_reads_bindings_or_errors() {
        let mut b = empty_bindings();
        b.insert(pname("threads"), Value::integer(pname("threads"), 8, None));
        let got = Expression::reference(pname("threads")).eval(&b).unwrap();
        assert_eq!(got, EvalValue::Integer(8));

        let err = Expression::reference(pname("missing")).eval(&b).unwrap_err();
        assert!(matches!(err, DerivationError::UnknownParameter(_)));
    }

    #[test]
    fn ref_rejects_selection_values() {
        use crate::value::SelectionItem;
        use indexmap::IndexSet;
        let mut set = IndexSet::new();
        set.insert(SelectionItem::new("a").unwrap());
        let mut b = empty_bindings();
        b.insert(pname("pick"), Value::selection(pname("pick"), set, None));
        let err = Expression::reference(pname("pick")).eval(&b).unwrap_err();
        assert_eq!(err, DerivationError::SelectionNotSupported);
    }

    // ---------- Arithmetic ----------

    #[test]
    fn integer_arithmetic() {
        let b = empty_bindings();
        let e = Expression::binop(
            BinOp::Add,
            Expression::literal(Literal::Integer { value: 3 }),
            Expression::literal(Literal::Integer { value: 4 }),
        );
        assert_eq!(e.eval(&b).unwrap(), EvalValue::Integer(7));

        let e = Expression::binop(
            BinOp::Mul,
            Expression::literal(Literal::Integer { value: 6 }),
            Expression::literal(Literal::Integer { value: 7 }),
        );
        assert_eq!(e.eval(&b).unwrap(), EvalValue::Integer(42));
    }

    #[test]
    fn double_arithmetic() {
        let b = empty_bindings();
        let e = Expression::binop(
            BinOp::Sub,
            Expression::literal(Literal::Double { value: 1.5 }),
            Expression::literal(Literal::Double { value: 0.5 }),
        );
        assert_eq!(e.eval(&b).unwrap(), EvalValue::Double(1.0));
    }

    #[test]
    fn integer_division_by_zero_errors() {
        let b = empty_bindings();
        let e = Expression::binop(
            BinOp::Div,
            Expression::literal(Literal::Integer { value: 1 }),
            Expression::literal(Literal::Integer { value: 0 }),
        );
        assert_eq!(e.eval(&b).unwrap_err(), DerivationError::DivisionByZero);
    }

    #[test]
    fn mod_rejects_non_integer() {
        let b = empty_bindings();
        let e = Expression::binop(
            BinOp::Mod,
            Expression::literal(Literal::Double { value: 1.0 }),
            Expression::literal(Literal::Double { value: 2.0 }),
        );
        assert!(matches!(
            e.eval(&b).unwrap_err(),
            DerivationError::TypeMismatch { .. }
        ));
    }

    // ---------- Comparison / boolean ----------

    #[test]
    fn comparisons_yield_boolean() {
        let b = empty_bindings();
        let e = Expression::binop(
            BinOp::Lt,
            Expression::literal(Literal::Integer { value: 3 }),
            Expression::literal(Literal::Integer { value: 4 }),
        );
        assert_eq!(e.eval(&b).unwrap(), EvalValue::Boolean(true));
    }

    #[test]
    fn equality_across_kinds() {
        let b = empty_bindings();
        let e = Expression::binop(
            BinOp::Eq,
            Expression::literal(Literal::String { value: "a".into() }),
            Expression::literal(Literal::String { value: "a".into() }),
        );
        assert_eq!(e.eval(&b).unwrap(), EvalValue::Boolean(true));
    }

    #[test]
    fn logical_and_or_and_not() {
        let b = empty_bindings();
        let t = Expression::literal(Literal::Boolean { value: true });
        let f = Expression::literal(Literal::Boolean { value: false });
        assert_eq!(
            Expression::binop(BinOp::And, t.clone(), f.clone()).eval(&b).unwrap(),
            EvalValue::Boolean(false)
        );
        assert_eq!(
            Expression::binop(BinOp::Or, t, f.clone()).eval(&b).unwrap(),
            EvalValue::Boolean(true)
        );
        assert_eq!(
            Expression::unop(UnOp::Not, f).eval(&b).unwrap(),
            EvalValue::Boolean(true)
        );
    }

    // ---------- Builtins ----------

    #[test]
    fn min_and_max() {
        let b = empty_bindings();
        let args = vec![
            Expression::literal(Literal::Integer { value: 3 }),
            Expression::literal(Literal::Integer { value: 1 }),
            Expression::literal(Literal::Integer { value: 2 }),
        ];
        assert_eq!(
            Expression::call(BuiltinFn::Min, args.clone()).eval(&b).unwrap(),
            EvalValue::Integer(1)
        );
        assert_eq!(
            Expression::call(BuiltinFn::Max, args).eval(&b).unwrap(),
            EvalValue::Integer(3)
        );
    }

    #[test]
    fn ceil_floor_round_return_integer() {
        let b = empty_bindings();
        assert_eq!(
            Expression::call(BuiltinFn::Ceil, vec![Expression::literal(Literal::Double { value: 1.2 })])
                .eval(&b)
                .unwrap(),
            EvalValue::Integer(2)
        );
        assert_eq!(
            Expression::call(BuiltinFn::Floor, vec![Expression::literal(Literal::Double { value: 1.9 })])
                .eval(&b)
                .unwrap(),
            EvalValue::Integer(1)
        );
        assert_eq!(
            Expression::call(BuiltinFn::Round, vec![Expression::literal(Literal::Double { value: 1.5 })])
                .eval(&b)
                .unwrap(),
            EvalValue::Integer(2)
        );
    }

    #[test]
    fn abs_pow_len() {
        let b = empty_bindings();
        assert_eq!(
            Expression::call(BuiltinFn::Abs, vec![Expression::literal(Literal::Integer { value: -5 })])
                .eval(&b)
                .unwrap(),
            EvalValue::Integer(5)
        );
        assert_eq!(
            Expression::call(
                BuiltinFn::Pow,
                vec![
                    Expression::literal(Literal::Double { value: 2.0 }),
                    Expression::literal(Literal::Double { value: 10.0 }),
                ]
            )
            .eval(&b)
            .unwrap(),
            EvalValue::Double(1024.0)
        );
        assert_eq!(
            Expression::call(
                BuiltinFn::Len,
                vec![Expression::literal(Literal::String { value: "hello".into() })]
            )
            .eval(&b)
            .unwrap(),
            EvalValue::Integer(5)
        );
    }

    #[test]
    fn arity_errors() {
        let b = empty_bindings();
        let err = Expression::call(BuiltinFn::Abs, vec![]).eval(&b).unwrap_err();
        assert!(matches!(err, DerivationError::InvalidArity { .. }));
        let err = Expression::call(BuiltinFn::Min, vec![Expression::literal(Literal::Integer { value: 1 })])
            .eval(&b)
            .unwrap_err();
        assert!(matches!(err, DerivationError::InvalidArity { .. }));
    }

    // ---------- If ----------

    #[test]
    fn if_expression_picks_branch() {
        let b = empty_bindings();
        let e = Expression::if_then_else(
            Expression::literal(Literal::Boolean { value: true }),
            Expression::literal(Literal::Integer { value: 1 }),
            Expression::literal(Literal::Integer { value: 2 }),
        );
        assert_eq!(e.eval(&b).unwrap(), EvalValue::Integer(1));
    }

    #[test]
    fn if_condition_must_be_boolean() {
        let b = empty_bindings();
        let e = Expression::if_then_else(
            Expression::literal(Literal::Integer { value: 1 }),
            Expression::literal(Literal::Integer { value: 1 }),
            Expression::literal(Literal::Integer { value: 2 }),
        );
        assert!(matches!(
            e.eval(&b).unwrap_err(),
            DerivationError::TypeMismatch { .. }
        ));
    }

    // ---------- serde ----------

    #[test]
    fn expression_serde_roundtrip() {
        let e = Expression::if_then_else(
            Expression::binop(
                BinOp::Lt,
                Expression::reference(pname("threads")),
                Expression::literal(Literal::Integer { value: 16 }),
            ),
            Expression::literal(Literal::Integer { value: 8 }),
            Expression::literal(Literal::Integer { value: 16 }),
        );
        let json = serde_json::to_string(&e).unwrap();
        let back: Expression = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }
}
