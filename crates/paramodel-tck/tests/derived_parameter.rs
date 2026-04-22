// Copyright (c) Jonathan Shook
// SPDX-License-Identifier: Apache-2.0

//! `DerivedParameter` + `Expression` evaluation coverage.

use paramodel_elements::{
    BinOp, BuiltinFn, DerivationError, DerivedParameter, Expression, Literal,
    ParameterName, UnOp, Value, ValueBindings, ValueKind,
};

fn pname(s: &str) -> ParameterName {
    ParameterName::new(s).unwrap()
}

fn bindings(pairs: &[(&str, Value)]) -> ValueBindings {
    let mut m = ValueBindings::new();
    for (k, v) in pairs {
        m.insert(pname(k), v.clone());
    }
    m
}

const fn lit_i(value: i64) -> Literal {
    Literal::Integer { value }
}
const fn lit_d(value: f64) -> Literal {
    Literal::Double { value }
}
const fn lit_b(value: bool) -> Literal {
    Literal::Boolean { value }
}
fn lit_s(value: &str) -> Literal {
    Literal::String {
        value: value.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Construction rejections.
// ---------------------------------------------------------------------------

#[test]
fn derived_parameter_rejects_selection_kind() {
    let err = DerivedParameter::new(
        pname("v"),
        ValueKind::Selection,
        Expression::literal(lit_i(1)),
    )
    .unwrap_err();
    let _ = err;
}

// ---------------------------------------------------------------------------
// Basic expression evaluation.
// ---------------------------------------------------------------------------

#[test]
fn add_two_integers() {
    let expr = Expression::binop(
        BinOp::Add,
        Expression::literal(lit_i(3)),
        Expression::literal(lit_i(4)),
    );
    let param = DerivedParameter::new(pname("sum"), ValueKind::Integer, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_integer(), Some(7));
}

#[test]
fn reference_bound_parameter() {
    let expr = Expression::binop(
        BinOp::Mul,
        Expression::reference(pname("base")),
        Expression::literal(lit_i(2)),
    );
    let param =
        DerivedParameter::new(pname("doubled"), ValueKind::Integer, expr).unwrap();
    let b = bindings(&[("base", Value::integer(pname("base"), 21, None))]);
    let v = param.compute(&b).unwrap();
    assert_eq!(v.as_integer(), Some(42));
}

// ---------------------------------------------------------------------------
// Division by zero.
// ---------------------------------------------------------------------------

#[test]
fn integer_division_by_zero_errors() {
    let expr = Expression::binop(
        BinOp::Div,
        Expression::literal(lit_i(10)),
        Expression::literal(lit_i(0)),
    );
    let param = DerivedParameter::new(pname("q"), ValueKind::Integer, expr).unwrap();
    let err = param.compute(&bindings(&[])).unwrap_err();
    // Division-by-zero surfaces as a DerivationError — any variant
    // is acceptable; test just ensures it errors rather than
    // panicking.
    let _ = err;
}

#[test]
fn integer_modulo_by_zero_errors() {
    let expr = Expression::binop(
        BinOp::Mod,
        Expression::literal(lit_i(10)),
        Expression::literal(lit_i(0)),
    );
    let param = DerivedParameter::new(pname("m"), ValueKind::Integer, expr).unwrap();
    let err = param.compute(&bindings(&[])).unwrap_err();
    let _ = err;
}

// ---------------------------------------------------------------------------
// Type mismatch.
// ---------------------------------------------------------------------------

#[test]
fn type_mismatch_when_expression_produces_wrong_kind() {
    // Declared kind = Integer, expression evaluates to Double → TypeMismatch.
    let expr = Expression::binop(
        BinOp::Add,
        Expression::literal(lit_d(1.0)),
        Expression::literal(lit_d(2.0)),
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let err = param.compute(&bindings(&[])).unwrap_err();
    assert!(matches!(err, DerivationError::TypeMismatch { .. }));
}

// ---------------------------------------------------------------------------
// Unresolved reference.
// ---------------------------------------------------------------------------

#[test]
fn ref_to_missing_binding_errors() {
    let expr = Expression::reference(pname("ghost"));
    let param =
        DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let err = param.compute(&bindings(&[])).unwrap_err();
    let _ = err;
}

// ---------------------------------------------------------------------------
// Unary ops.
// ---------------------------------------------------------------------------

#[test]
fn neg_unary_on_integer() {
    let expr = Expression::unop(UnOp::Neg, Expression::literal(lit_i(7)));
    let param =
        DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_integer(), Some(-7));
}

#[test]
fn not_unary_on_boolean() {
    let expr = Expression::unop(UnOp::Not, Expression::literal(lit_b(true)));
    let param =
        DerivedParameter::new(pname("v"), ValueKind::Boolean, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_boolean(), Some(false));
}

// ---------------------------------------------------------------------------
// If-then-else.
// ---------------------------------------------------------------------------

#[test]
fn if_then_else_selects_then_branch_on_true() {
    let expr = Expression::if_then_else(
        Expression::literal(lit_b(true)),
        Expression::literal(lit_i(100)),
        Expression::literal(lit_i(200)),
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_integer(), Some(100));
}

#[test]
fn if_then_else_selects_else_branch_on_false() {
    let expr = Expression::if_then_else(
        Expression::literal(lit_b(false)),
        Expression::literal(lit_i(100)),
        Expression::literal(lit_i(200)),
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_integer(), Some(200));
}

// ---------------------------------------------------------------------------
// Builtins.
// ---------------------------------------------------------------------------

#[test]
fn builtin_abs_on_negative_integer() {
    let expr = Expression::call(
        BuiltinFn::Abs,
        vec![Expression::literal(lit_i(-42))],
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_integer(), Some(42));
}

#[test]
fn builtin_min_returns_smallest_of_two() {
    let expr = Expression::call(
        BuiltinFn::Min,
        vec![
            Expression::literal(lit_i(5)),
            Expression::literal(lit_i(3)),
        ],
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_integer(), Some(3));
}

#[test]
fn builtin_max_returns_largest_of_two() {
    let expr = Expression::call(
        BuiltinFn::Max,
        vec![
            Expression::literal(lit_i(5)),
            Expression::literal(lit_i(3)),
        ],
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_integer(), Some(5));
}

#[test]
fn builtin_len_of_string_is_byte_length() {
    let expr = Expression::call(
        BuiltinFn::Len,
        vec![Expression::literal(lit_s("hello"))],
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Integer, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_integer(), Some(5));
}

// ---------------------------------------------------------------------------
// Logical short-circuit and relation ops.
// ---------------------------------------------------------------------------

#[test]
fn comparison_returns_boolean() {
    let expr = Expression::binop(
        BinOp::Lt,
        Expression::literal(lit_i(3)),
        Expression::literal(lit_i(5)),
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Boolean, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_boolean(), Some(true));
}

#[test]
fn logical_and_combines_booleans() {
    let expr = Expression::binop(
        BinOp::And,
        Expression::literal(lit_b(true)),
        Expression::literal(lit_b(false)),
    );
    let param = DerivedParameter::new(pname("v"), ValueKind::Boolean, expr).unwrap();
    let v = param.compute(&bindings(&[])).unwrap();
    assert_eq!(v.as_boolean(), Some(false));
}
