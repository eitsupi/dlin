//! Operations on known values, delegated to minijinja.
//!
//! The interpreter evaluates operands itself and asks minijinja for one
//! operation on already evaluated values, so the result follows minijinja's
//! semantics without re-evaluating a sub-expression (which could repeat a
//! `ref()` call or lose an unknown value).

use std::collections::HashMap;
use std::sync::OnceLock;

use minijinja::value::Value;
use minijinja::{Environment, Expression};

use super::ir::BinOp;

/// Work allowed for one delegated operation (minijinja's fuel also counts
/// memory and the work inside filters). An operation that runs out fails,
/// and the interpreter treats its result as unknown.
const FUEL_PER_OPERATION: u64 = 100_000;

fn environment() -> &'static Environment<'static> {
    static ENV: OnceLock<Environment<'static>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.set_fuel(Some(FUEL_PER_OPERATION));
        // Python-compatible methods (`'a'.replace(...)`, `d.items()`), as
        // dbt Fusion's environment uses.
        env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
        env
    })
}

const SOURCES: &[&str] = &[
    "a == b",
    "a != b",
    "a < b",
    "a <= b",
    "a > b",
    "a >= b",
    "a and b",
    "a or b",
    "a + b",
    "a - b",
    "a * b",
    "a / b",
    "a // b",
    "a % b",
    "a ** b",
    "a ~ b",
    "a in b",
    "a not in b",
    "-a",
    "v[a:b:c]",
    "v[k]",
];

/// The fixed expressions, compiled once.
fn expression(source: &'static str) -> &'static Expression<'static, 'static> {
    static CACHE: OnceLock<HashMap<&'static str, Expression<'static, 'static>>> = OnceLock::new();
    CACHE
        .get_or_init(|| {
            SOURCES
                .iter()
                .map(|source| {
                    let compiled = environment()
                        .compile_expression(source)
                        .expect("fixed expression compiles");
                    (*source, compiled)
                })
                .collect()
        })
        .get(source)
        .expect("expression is listed in SOURCES")
}

/// Apply a binary operator.
pub(crate) fn binary(op: BinOp, left: &Value, right: &Value) -> Result<Value, minijinja::Error> {
    let source = match op {
        BinOp::Eq => "a == b",
        BinOp::Ne => "a != b",
        BinOp::Lt => "a < b",
        BinOp::Lte => "a <= b",
        BinOp::Gt => "a > b",
        BinOp::Gte => "a >= b",
        BinOp::And => "a and b",
        BinOp::Or => "a or b",
        BinOp::Add => "a + b",
        BinOp::Sub => "a - b",
        BinOp::Mul => "a * b",
        BinOp::Div => "a / b",
        BinOp::FloorDiv => "a // b",
        BinOp::Rem => "a % b",
        BinOp::Pow => "a ** b",
        BinOp::Concat => "a ~ b",
        BinOp::In => "a in b",
        BinOp::NotIn => "a not in b",
    };
    expression(source).eval(Value::from_pairs([
        ("a", left.clone()),
        ("b", right.clone()),
    ]))
}

pub(crate) fn negate(value: &Value) -> Result<Value, minijinja::Error> {
    expression("-a").eval(Value::from_pairs([("a", value.clone())]))
}

pub(crate) fn slice(
    value: &Value,
    start: &Value,
    stop: &Value,
    step: &Value,
) -> Result<Value, minijinja::Error> {
    expression("v[a:b:c]").eval(Value::from_pairs([
        ("v", value.clone()),
        ("a", start.clone()),
        ("b", stop.clone()),
        ("c", step.clone()),
    ]))
}

pub(crate) fn get_item(value: &Value, key: &Value) -> Result<Value, minijinja::Error> {
    expression("v[k]").eval(Value::from_pairs([
        ("v", value.clone()),
        ("k", key.clone()),
    ]))
}

pub(crate) fn render(value: &Value) -> String {
    value.to_string()
}

/// Apply a built-in filter to known arguments. `None` when the filter does
/// not exist.
pub(crate) fn filter(name: &str, args: &[Value]) -> Option<Result<Value, minijinja::Error>> {
    let mut state = environment().empty_state();
    match state.apply_filter(name, args) {
        Err(error) if error.kind() == minijinja::ErrorKind::UnknownFilter => None,
        result => Some(result),
    }
}

/// Perform a built-in test on known arguments. `None` when the test does
/// not exist.
pub(crate) fn test(name: &str, args: &[Value]) -> Option<Result<bool, minijinja::Error>> {
    let mut state = environment().empty_state();
    match state.perform_test(name, args) {
        Err(error) if error.kind() == minijinja::ErrorKind::UnknownTest => None,
        result => Some(result),
    }
}

/// Call a pure minijinja global function (`range`, `dict`) on known
/// arguments.
pub(crate) fn call_global(name: &str, args: &[Value]) -> Result<Value, minijinja::Error> {
    let mut state = environment().empty_state();
    let function = state.lookup(name).ok_or_else(|| {
        minijinja::Error::new(minijinja::ErrorKind::UnknownFunction, name.to_string())
    })?;
    function.call(&mut state, args)
}

/// Call a method of a known value. `None` when the method does not exist
/// or fails for these arguments.
pub(crate) fn call_method(value: &Value, name: &str, args: &[Value]) -> Option<Value> {
    let mut state = environment().empty_state();
    value.call_method(&mut state, name, args).ok()
}
