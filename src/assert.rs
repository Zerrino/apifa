//! Assertions on a response, like Bruno's Assert tab: rows of `expression · operator · value`
//! (`res.status eq 200`, `res.body.length gt 0`, `res.body[0].login isString`), checked after every
//! response of a request tab, a flow's request block, or the test runner (`apifa test`).
//!
//! The expression is a path in `res` = `{status, headers, body, time, size}` (`res.` may be left out;
//! header names are lowercase: `res.headers.x-total`). The value is text, parsed as JSON when it
//! looks like it (`200`, `true`, `[1,2]`); `{{templates}}` work (flow variables, `{{now}}`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::flow::{lookup, loose_value, to_text};
use crate::net::ResponseData;

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Assertion {
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub enabled: bool,
    pub expr: String,
    #[serde(default)]
    pub op: Op,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
}

impl Assertion {
    pub fn new(expr: &str, op: Op, value: &str) -> Self {
        Self { enabled: true, expr: expr.into(), op, value: value.into() }
    }

    /// `res.status eq 200`
    pub fn text(&self) -> String {
        if self.op.unary() {
            format!("{} {}", self.expr.trim(), self.op.id())
        } else {
            format!("{} {} {}", self.expr.trim(), self.op.id(), self.value.trim())
        }
    }
}

/// Bruno's operators, same names.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    #[default]
    Eq,
    Neq,
    Gt,
    Gte,
    Lt,
    Lte,
    In,
    NotIn,
    Contains,
    NotContains,
    Length,
    Matches,
    NotMatches,
    StartsWith,
    EndsWith,
    Between,
    IsEmpty,
    IsNotEmpty,
    IsNull,
    IsUndefined,
    IsDefined,
    IsTruthy,
    IsFalsy,
    IsJson,
    IsNumber,
    IsString,
    IsBoolean,
    IsArray,
}

impl Op {
    pub const ALL: [Op; 28] = [
        Op::Eq,
        Op::Neq,
        Op::Gt,
        Op::Gte,
        Op::Lt,
        Op::Lte,
        Op::In,
        Op::NotIn,
        Op::Contains,
        Op::NotContains,
        Op::Length,
        Op::Matches,
        Op::NotMatches,
        Op::StartsWith,
        Op::EndsWith,
        Op::Between,
        Op::IsEmpty,
        Op::IsNotEmpty,
        Op::IsNull,
        Op::IsUndefined,
        Op::IsDefined,
        Op::IsTruthy,
        Op::IsFalsy,
        Op::IsJson,
        Op::IsNumber,
        Op::IsString,
        Op::IsBoolean,
        Op::IsArray,
    ];

    /// The name in files (`notIn`).
    pub fn id(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
    }

    pub fn unary(self) -> bool {
        matches!(
            self,
            Op::IsEmpty
                | Op::IsNotEmpty
                | Op::IsNull
                | Op::IsUndefined
                | Op::IsDefined
                | Op::IsTruthy
                | Op::IsFalsy
                | Op::IsJson
                | Op::IsNumber
                | Op::IsString
                | Op::IsBoolean
                | Op::IsArray
        )
    }

    pub fn help(self) -> &'static str {
        match self {
            Op::Eq => "equals (numbers as numbers: 200 = \"200\")",
            Op::Neq => "is not equal to",
            Op::Gt | Op::Gte | Op::Lt | Op::Lte => "numbers, or dates as dates",
            Op::In | Op::NotIn => "one of a list: a,b,c or [1,2]",
            Op::Contains | Op::NotContains => "text contains, list has the element, object has the key",
            Op::Length => "a list / text / object has this many elements",
            Op::Matches | Op::NotMatches => "regular expression, e.g. ^[a-z]+$",
            Op::StartsWith | Op::EndsWith => "text starts / ends with",
            Op::Between => "min,max (included), numbers or dates",
            Op::IsEmpty | Op::IsNotEmpty => "\"\", [], {}, null or missing",
            Op::IsNull => "null",
            Op::IsUndefined | Op::IsDefined => "the field is missing / present",
            Op::IsTruthy | Op::IsFalsy => "like JavaScript: false, 0, \"\", null and missing are falsy",
            Op::IsJson => "an object or a list",
            Op::IsNumber | Op::IsString | Op::IsBoolean | Op::IsArray => "type of the value",
        }
    }
}

#[derive(Clone)]
pub struct Outcome {
    /// `res.status eq 200` (templates of the value filled in).
    pub text: String,
    pub passed: bool,
    /// Why it failed: `got 404`.
    pub detail: String,
}

/// `res` for a response: `{status, headers (lowercase names), body (JSON when it is), time (ms), size}`.
pub fn context(r: &ResponseData) -> Map<String, Value> {
    let headers: Map<String, Value> = r.headers.iter().map(|(k, v)| (k.to_lowercase(), Value::String(v.clone()))).collect();
    let body = serde_json::from_str::<Value>(&r.body).unwrap_or_else(|_| Value::String(r.body.clone()));
    res_context(r.status, headers, body, r.elapsed.as_millis() as u64, r.size)
}

pub fn res_context(status: u16, headers: Map<String, Value>, body: Value, time: u64, size: usize) -> Map<String, Value> {
    let res = serde_json::json!({ "status": status, "headers": headers, "body": body, "time": time, "size": size });
    let mut m = Map::new();
    m.insert("res".into(), res);
    m
}

/// Checks the enabled rows; `fill` renders the `{{templates}}` of a value.
pub fn check(rows: &[Assertion], ctx: &Map<String, Value>, fill: &dyn Fn(&str) -> String) -> Vec<Outcome> {
    rows.iter().filter(|a| a.enabled && !a.expr.trim().is_empty()).map(|a| check_one(a, ctx, fill)).collect()
}

fn check_one(a: &Assertion, ctx: &Map<String, Value>, fill: &dyn Fn(&str) -> String) -> Outcome {
    let expr = a.expr.trim().trim_start_matches("{{").trim_end_matches("}}").trim();
    let path = if expr == "res" || expr.starts_with("res.") || expr.starts_with("res[") { expr.to_owned() } else { format!("res.{expr}") };
    let actual = lookup(ctx, &path);
    let value = fill(&a.value);
    let shown = Assertion { value: value.clone(), ..a.clone() }.text();
    match eval(a.op, actual.as_ref(), value.trim()) {
        Ok(()) => Outcome { text: shown, passed: true, detail: String::new() },
        Err(detail) => Outcome { text: shown, passed: false, detail },
    }
}

fn got(v: Option<&Value>) -> String {
    match v {
        None => "the field is missing".into(),
        Some(v) => {
            let s = match v {
                Value::String(s) => format!("\"{s}\""),
                other => other.to_string(),
            };
            let s: String = if s.chars().count() > 80 { format!("{}…", s.chars().take(80).collect::<String>()) } else { s };
            format!("got {s}")
        }
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// `200` = `"200"`, `true` = `"true"`, otherwise JSON equality, then text.
fn equal(actual: &Value, expected: &Value) -> bool {
    if let (Some(a), Some(b)) = (number(actual), number(expected)) {
        return a == b;
    }
    actual == expected || (!actual.is_object() && !expected.is_object() && to_text(actual) == to_text(expected))
}

/// Order of two values: numbers, else dates, else none.
fn order(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if let (Some(x), Some(y)) = (number(a), number(b)) {
        return x.partial_cmp(&y);
    }
    let (x, y) = (crate::dates::parse(&to_text(a))?, crate::dates::parse(&to_text(b))?);
    Some(x.cmp(&y))
}

/// `a,b,c` or a JSON list.
fn list(value: &str) -> Vec<Value> {
    match loose_value(value) {
        Value::Array(a) => a,
        _ => value.split(',').map(|s| loose_value(s.trim())).collect(),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn empty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        Some(_) => false,
    }
}

fn regex(pattern: &str) -> Result<regex::Regex, String> {
    // `/^ab/i` written the JavaScript way works too.
    let (pat, flags) = match pattern.strip_prefix('/').and_then(|p| p.rsplit_once('/')) {
        Some((p, f)) if f.chars().all(|c| "imsx".contains(c)) => (p, f),
        _ => (pattern, ""),
    };
    let full = if flags.is_empty() { pat.to_owned() } else { format!("(?{flags}){pat}") };
    regex::Regex::new(&full).map_err(|e| format!("bad regular expression: {e}"))
}

fn eval(op: Op, actual: Option<&Value>, value: &str) -> Result<(), String> {
    let fail = || Err(got(actual));
    let expected = loose_value(value);
    let ok = |b: bool| if b { Ok(()) } else { fail() };
    match op {
        Op::IsUndefined => return ok(actual.is_none()),
        Op::IsDefined => return ok(actual.is_some()),
        Op::IsEmpty => return ok(empty(actual)),
        Op::IsNotEmpty => return ok(!empty(actual)),
        Op::IsTruthy => return ok(truthy(actual)),
        Op::IsFalsy => return ok(!truthy(actual)),
        _ => {}
    }
    let Some(a) = actual else { return fail() };
    let text = to_text(a);
    match op {
        Op::Eq => ok(equal(a, &expected)),
        Op::Neq => ok(!equal(a, &expected)),
        Op::Gt | Op::Gte | Op::Lt | Op::Lte => match order(a, &expected) {
            None => Err(format!("{}: not comparable with {value}", got(actual))),
            Some(o) => ok(match op {
                Op::Gt => o.is_gt(),
                Op::Gte => o.is_ge(),
                Op::Lt => o.is_lt(),
                _ => o.is_le(),
            }),
        },
        Op::In => ok(list(value).iter().any(|e| equal(a, e))),
        Op::NotIn => ok(!list(value).iter().any(|e| equal(a, e))),
        Op::Contains | Op::NotContains => {
            let has = match a {
                Value::Array(items) => items.iter().any(|e| equal(e, &expected)),
                Value::Object(o) => o.contains_key(value),
                _ => text.contains(value),
            };
            ok(has == (op == Op::Contains))
        }
        Op::Length => {
            let n = match a {
                Value::Array(items) => items.len(),
                Value::Object(o) => o.len(),
                Value::String(s) => s.chars().count(),
                _ => return Err(format!("{}: has no length", got(actual))),
            };
            if number(&expected) == Some(n as f64) {
                Ok(())
            } else {
                Err(format!("length is {n}"))
            }
        }
        Op::Matches => ok(regex(value)?.is_match(&text)),
        Op::NotMatches => ok(!regex(value)?.is_match(&text)),
        Op::StartsWith => ok(text.starts_with(value)),
        Op::EndsWith => ok(text.ends_with(value)),
        Op::Between => {
            let bounds = list(value);
            let [lo, hi] = bounds.as_slice() else { return Err("between expects min,max".into()) };
            match (order(a, lo), order(a, hi)) {
                (Some(x), Some(y)) => ok(x.is_ge() && y.is_le()),
                _ => Err(format!("{}: not comparable with {value}", got(actual))),
            }
        }
        Op::IsNull => ok(a.is_null()),
        Op::IsJson => ok(a.is_object() || a.is_array()),
        Op::IsNumber => ok(a.is_number()),
        Op::IsString => ok(a.is_string()),
        Op::IsBoolean => ok(a.is_boolean()),
        Op::IsArray => ok(a.is_array()),
        Op::IsUndefined | Op::IsDefined | Op::IsEmpty | Op::IsNotEmpty | Op::IsTruthy | Op::IsFalsy => unreachable!(),
    }
}

/// Problems `check` reports: an empty expression, a missing value, a bad regular expression.
pub fn lint(rows: &[Assertion], at: &str, out: &mut Vec<String>) {
    for (i, a) in rows.iter().enumerate() {
        let here = format!("{at}assertion {}", i + 1);
        if a.expr.trim().is_empty() {
            out.push(format!("{here}: empty “expr”"));
        }
        if !a.op.unary() && a.value.trim().is_empty() && a.op != Op::Eq && a.op != Op::Neq {
            out.push(format!("{here}: “{}” needs a value", a.op.id()));
        }
        if matches!(a.op, Op::Matches | Op::NotMatches) && !a.value.contains("{{") {
            if let Err(e) = regex(a.value.trim()) {
                out.push(format!("{here}: {e}"));
            }
        }
        if a.op == Op::Between && list(&a.value).len() != 2 && !a.value.contains("{{") {
            out.push(format!("{here}: between expects min,max"));
        }
    }
}

/// `3/4 passed`
pub fn summary(outcomes: &[Outcome]) -> String {
    let passed = outcomes.iter().filter(|o| o.passed).count();
    format!("{passed}/{} passed", outcomes.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> Map<String, Value> {
        let headers = json!({ "x-total": "40", "content-type": "application/json" });
        let body = json!([{ "id": 1, "login": "jdoe", "email": "jdoe@example.com", "active?": true, "image": null, "created_at": "2026-10-12T08:00:00.000Z" }, { "id": 2, "login": "asmith" }]);
        res_context(200, headers.as_object().unwrap().clone(), body, 312, 1234)
    }

    fn pass(expr: &str, op: Op, value: &str) -> bool {
        check(&[Assertion::new(expr, op, value)], &ctx(), &|v| v.to_owned())[0].passed
    }

    #[test]
    fn operators() {
        assert!(pass("res.status", Op::Eq, "200"));
        assert!(pass("status", Op::Eq, "200"), "res. may be left out");
        assert!(pass("{{res.status}}", Op::Eq, "200"));
        assert!(!pass("res.status", Op::Eq, "201"));
        assert!(pass("res.status", Op::Neq, "404"));
        assert!(pass("res.headers.x-total", Op::Gt, "38"), "header text compares as a number");
        assert!(pass("res.time", Op::Lt, "1000"));
        assert!(pass("res.body.length", Op::Eq, "2"));
        assert!(pass("res.body", Op::Length, "2"));
        assert!(pass("res.body[0].login", Op::In, "jdoe,bob"));
        assert!(pass("res.body[0].id", Op::NotIn, "[5, 6]"));
        assert!(pass("res.body.login", Op::Contains, "asmith"), "a field on a list collects it");
        assert!(pass("res.body[0]", Op::Contains, "email"), "an object contains its keys");
        assert!(pass("res.body[0].email", Op::Matches, r"^[a-z]+@example\.com$"));
        assert!(pass("res.body[0].login", Op::Matches, "/^JD/i"));
        assert!(pass("res.body[0].login", Op::NotMatches, "^x"));
        assert!(pass("res.body[0].email", Op::EndsWith, "example.com"));
        assert!(pass("res.body[0].login", Op::StartsWith, "jd"));
        assert!(pass("res.body[0].id", Op::Between, "1,10"));
        assert!(pass("res.body[0].created_at", Op::Gt, "2026-10-01"), "dates compare as dates");
        assert!(pass("res.body[0].created_at", Op::Between, "2026-10-01,2026-10-31"));
        assert!(pass("res.body[0].image", Op::IsNull, ""));
        assert!(pass("res.body[0].image", Op::IsEmpty, ""));
        assert!(pass("res.body[1].email", Op::IsUndefined, ""));
        assert!(!pass("res.body[1].email", Op::IsDefined, ""));
        assert!(pass("res.body[0].active?", Op::IsTruthy, ""));
        assert!(pass("res.body[1].image", Op::IsFalsy, ""));
        assert!(pass("res.body", Op::IsJson, ""));
        assert!(pass("res.body", Op::IsArray, ""));
        assert!(pass("res.body[0].id", Op::IsNumber, ""));
        assert!(pass("res.body[0].login", Op::IsString, ""));
        assert!(pass("res.body[0].active?", Op::IsBoolean, ""));
        assert!(pass("res.body", Op::IsNotEmpty, ""));
    }

    #[test]
    fn failures_say_what_was_there() {
        let out = check(
            &[
                Assertion::new("res.body[0].login", Op::Eq, "{{who}}"),
                Assertion::new("res.nope", Op::Eq, "1"),
                Assertion { enabled: false, ..Assertion::new("res.status", Op::Eq, "500") },
            ],
            &ctx(),
            &|v| v.replace("{{who}}", "bob"),
        );
        assert_eq!(out.len(), 2, "disabled rows are skipped");
        assert_eq!(out[0].text, "res.body[0].login eq bob");
        assert_eq!(out[0].detail, "got \"jdoe\"");
        assert_eq!(out[1].detail, "the field is missing");
        assert_eq!(summary(&out), "0/2 passed");
    }

    #[test]
    fn file_format() {
        let a: Assertion = serde_json::from_value(json!({ "expr": "res.body", "op": "isNotEmpty" })).unwrap();
        assert_eq!(a.op, Op::IsNotEmpty);
        assert!(a.enabled);
        assert_eq!(serde_json::to_string(&Assertion::new("res.status", Op::NotIn, "500,502")).unwrap(), r#"{"expr":"res.status","op":"notIn","value":"500,502"}"#);
        assert_eq!(Op::ALL.iter().map(|o| o.id()).collect::<std::collections::HashSet<_>>().len(), 28);
    }
}
