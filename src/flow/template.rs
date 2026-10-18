//! The `{{template}}` engine: variable paths, rendering text and JSON, comparisons.

use std::borrow::Cow;
use std::collections::HashSet;

use serde_json::{Map, Value};

use std::collections::BTreeMap;

use crate::model;

use super::*;

// ==================================================================== templates

pub(super) enum Seg {
    Key(String),
    Index(i64),
}

pub(super) fn parse_path(expr: &str) -> Vec<Seg> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut chars = expr.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !cur.is_empty() {
                    segs.push(Seg::Key(std::mem::take(&mut cur)));
                }
            }
            '[' => {
                if !cur.is_empty() {
                    segs.push(Seg::Key(std::mem::take(&mut cur)));
                }
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    inner.push(c);
                }
                let inner = inner.trim().trim_matches(|c| c == '"' || c == '\'');
                match inner.parse::<i64>() {
                    Ok(i) => segs.push(Seg::Index(i)),
                    Err(_) => segs.push(Seg::Key(inner.to_owned())),
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        segs.push(Seg::Key(cur));
    }
    segs
}

pub(super) fn index(v: &Value, i: i64) -> Option<&Value> {
    let arr = v.as_array()?;
    let i = if i < 0 { arr.len() as i64 + i } else { i };
    arr.get(usize::try_from(i).ok()?)
}

/// Strips optional `{{ }}` around an expression.
pub fn bare(expr: &str) -> &str {
    let t = expr.trim();
    t.strip_prefix("{{").and_then(|s| s.strip_suffix("}}")).map(str::trim).unwrap_or(t)
}

/// Evaluates a variable path such as `user.body.orders[0].total`.
pub fn lookup(vars: &Map<String, Value>, expr: &str) -> Option<Value> {
    let segs = parse_path(bare(expr));
    let mut it = segs.into_iter();
    // Borrowed while walking down: a big response isn't copied at each step, only the value found.
    let mut cur: Cow<Value> = match it.next()? {
        Seg::Key(k) => Cow::Borrowed(vars.get(&k)?),
        Seg::Index(_) => return None,
    };
    for seg in it {
        cur = match seg {
            Seg::Index(i) => sub(cur, |c| index(c, i))?,
            Seg::Key(k) => match cur.as_ref() {
                Value::Object(_) => sub(cur, |c| c.get(&k))?,
                Value::Array(a) if matches!(k.as_str(), "length" | "len" | "count") => Cow::Owned(Value::from(a.len())),
                // Without duplicates, first ones kept: visits.body.user.login.unique
                Value::Array(a) if k == "unique" => {
                    let mut seen = HashSet::new();
                    Cow::Owned(Value::Array(a.iter().filter(|v| seen.insert(canonical(v))).cloned().collect()))
                }
                Value::String(s) if matches!(k.as_str(), "length" | "len") => Cow::Owned(Value::from(s.chars().count())),
                Value::Array(a) => match k.parse::<i64>() {
                    Ok(i) => sub(cur, |c| index(c, i))?,
                    // A field on a list collects it from every element: users.body.login
                    Err(_) => Cow::Owned(Value::Array(a.iter().filter_map(|e| e.get(&k).cloned()).collect())),
                },
                _ => return None,
            },
        };
    }
    Some(cur.into_owned())
}

/// Text that is the same for equal values, whatever the order of their keys (`.unique`).
pub(super) fn canonical(v: &Value) -> String {
    match v {
        Value::Object(o) => {
            let mut fields: Vec<(&String, String)> = o.iter().map(|(k, v)| (k, canonical(v))).collect();
            fields.sort();
            let inner: Vec<String> = fields.into_iter().map(|(k, v)| format!("{}:{v}", Value::from(k.as_str()))).collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::Array(a) => format!("[{}]", a.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}

/// A part of `cur`: still borrowed from the variables when `cur` is.
pub(super) fn sub<'a>(cur: Cow<'a, Value>, pick: impl Fn(&Value) -> Option<&Value>) -> Option<Cow<'a, Value>> {
    match cur {
        Cow::Borrowed(v) => pick(v).map(Cow::Borrowed),
        Cow::Owned(v) => pick(&v).cloned().map(Cow::Owned),
    }
}

/// A variable, or a built-in dynamic date (`now`, `now-7d`, `today`…) computed right now.
pub fn resolve(vars: &Map<String, Value>, expr: &str) -> Option<Value> {
    lookup(vars, expr).or_else(|| crate::dates::dynamic(bare(expr)).map(Value::String))
}

/// Text form of a value: strings as-is, lists of scalars comma-joined (`filter[x]=a,b`).
pub fn to_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Array(a) if a.iter().all(|e| !e.is_object() && !e.is_array()) => a.iter().map(to_text).collect::<Vec<_>>().join(","),
        other => other.to_string(),
    }
}

/// Replaces every `{{expr}}`; `strict` makes unknown variables an error.
pub(super) fn render_with(tpl: &str, vars: &Map<String, Value>, strict: bool, mut emit: impl FnMut(&mut String, Option<&Value>)) -> Result<String, String> {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let expr = after[..end].trim();
        let value = resolve(vars, expr);
        if value.is_none() && strict {
            return Err(format!("unknown variable {{{{{expr}}}}}"));
        }
        emit(&mut out, value.as_ref());
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

pub fn render(tpl: &str, vars: &Map<String, Value>) -> Result<String, String> {
    render_with(tpl, vars, true, |out, v| out.push_str(&v.map(to_text).unwrap_or_default()))
}

pub fn render_lenient(tpl: &str, vars: &Map<String, Value>) -> String {
    render_with(tpl, vars, false, |out, v| out.push_str(&v.map(to_text).unwrap_or_default())).unwrap_or_default()
}

/// Renders a JSON template: `{{x}}` inside a string inserts text, outside inserts the JSON value.
pub fn render_json(tpl: &str, vars: &Map<String, Value>) -> Result<Option<Value>, String> {
    if tpl.trim().is_empty() {
        return Ok(None);
    }
    let mut out = String::with_capacity(tpl.len());
    let mut in_string = false;
    let mut escaped = false;
    let bytes = tpl.as_bytes();
    let mut i = 0;
    while i < tpl.len() {
        if tpl[i..].starts_with("{{") {
            if let Some(end) = tpl[i + 2..].find("}}") {
                let expr = tpl[i + 2..i + 2 + end].trim();
                let v = resolve(vars, expr).ok_or_else(|| format!("unknown variable {{{{{expr}}}}} in the body"))?;
                if in_string {
                    let quoted = serde_json::to_string(&to_text(&v)).unwrap_or_default();
                    out.push_str(&quoted[1..quoted.len() - 1]);
                } else {
                    out.push_str(&v.to_string());
                }
                i += 2 + end + 2;
                continue;
            }
        }
        let c = tpl[i..].chars().next().unwrap_or(' ');
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if bytes[i] == b'"' {
            in_string = true;
        }
        out.push(c);
        i += c.len_utf8();
    }
    serde_json::from_str(&out).map(Some).map_err(|e| format!("body is not valid JSON after substitution: {e}"))
}

/// `{{expr}}` and nothing else around it.
pub(super) fn single_placeholder(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("{{") && t.ends_with("}}") && t.matches("{{").count() == 1
}

/// An output expression: `{{path}}` keeps the value's type, text with `{{…}}` inside gives text,
/// a bare path is looked up. Unknown variables give `null`.
pub fn eval_output(expr: &str, vars: &Map<String, Value>) -> Value {
    let t = expr.trim();
    if t.is_empty() {
        Value::Null
    } else if single_placeholder(t) || !t.contains("{{") {
        resolve(vars, t).unwrap_or(Value::Null)
    } else {
        Value::String(render_lenient(t, vars))
    }
}

/// Deep merge: `over` wins, objects are merged key by key.
pub(super) fn merge_json(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(existing) => merge_json(existing, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

/// Parses user text into a JSON value when it looks like one (numbers, booleans, lists…).
pub fn loose_value(s: &str) -> Value {
    serde_json::from_str::<Value>(s.trim()).ok().filter(|v| !v.is_string()).unwrap_or_else(|| Value::String(s.to_owned()))
}

pub(super) fn compare(op: CompareOp, a: &str, b: &str) -> bool {
    let num = || Some((a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?));
    // Two dates compare as moments (2026-10-12 vs 2026-10-14T08:00:00.000+02:00), not as text.
    let dates = || Some((crate::dates::parse(a)?, crate::dates::parse(b)?));
    let order = || match (num(), dates()) {
        (Some((x, y)), _) => x.partial_cmp(&y),
        (None, Some((x, y))) => Some(x.cmp(&y)),
        _ => Some(a.cmp(b)),
    };
    match op {
        CompareOp::Eq => num().map(|(x, y)| x == y).unwrap_or(a == b),
        CompareOp::Ne => !num().map(|(x, y)| x == y).unwrap_or(a == b),
        CompareOp::Contains => a.contains(b),
        // An empty side (a field that doesn't exist) is never greater or smaller.
        CompareOp::Gt => !a.trim().is_empty() && !b.trim().is_empty() && order() == Some(std::cmp::Ordering::Greater),
        CompareOp::Lt => !a.trim().is_empty() && !b.trim().is_empty() && order() == Some(std::cmp::Ordering::Less),
        CompareOp::NotEmpty => !a.trim().is_empty() && a != "[]" && a != "{}",
        CompareOp::Empty => a.trim().is_empty() || a == "[]" || a == "{}",
    }
}

/// Renders `{{vars}}` then fills `:name` segments from variables of the same name.
#[cfg(test)]
pub fn resolve_path(template: &str, vars: &Map<String, Value>) -> Result<String, String> {
    resolve_path_with(template, vars, &mut |_, v| Ok(v))
}

/// [`resolve_path`], with `fix(":name", value)` applied to each `:name` value (substitutions).
pub fn resolve_path_with(template: &str, vars: &Map<String, Value>, fix: &mut dyn FnMut(&str, String) -> Result<String, String>) -> Result<String, String> {
    let rendered = render_with(template.trim(), vars, true, |out, v| out.push_str(&model::encode_component(&v.map(to_text).unwrap_or_default())))?;
    // An absolute URL keeps its origin aside (`:8080` isn't a path variable).
    let (origin, rendered) = model::split_origin(&rendered);
    let mut values = BTreeMap::new();
    for v in model::path_vars(rendered) {
        if let Some(val) = lookup(vars, &v.name) {
            let fixed = fix(&format!(":{}", v.name), to_text(&val))?;
            values.insert(v.name, fixed);
        }
    }
    let (path, missing) = model::render_path(rendered, &values, true);
    if let Some(m) = missing.first() {
        return Err(format!("path variable :{m} has no value (create a variable named {m})"));
    }
    Ok(format!("{origin}{}", if path.starts_with('/') { path } else { format!("/{path}") }))
}

/// Every leaf of a JSON value as (`a.b[0].c`, value) — the path can be pasted as `{{a.b[0].c}}`.
pub fn flatten(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(o) if !o.is_empty() => {
            for (k, child) in o {
                let path = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
                flatten(&path, child, out);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            for (i, child) in a.iter().enumerate() {
                flatten(&format!("{prefix}[{i}]"), child, out);
            }
        }
        other => out.push((prefix.to_owned(), other.to_string())),
    }
}
