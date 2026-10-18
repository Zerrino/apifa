//! Pre-request and post-response scripts: JavaScript run in QuickJS (`rquickjs`), with a subset of
//! Postman's `pm` API (`pm.environment`, `pm.variables`, `pm.globals`, `pm.request`, `pm.response`,
//! `pm.test`, `pm.expect`, `console`, `postman.*`).
//!
//! Each [`run`] gets a fresh runtime: nothing is kept between calls. Scripts have no network and no
//! file system (`pm.sendRequest` and `require` throw), a time limit (interrupt handler) and a memory
//! limit. The whole `pm` API is a JavaScript prelude ([`PRELUDE`]): it takes the input as one JSON value
//! and hands back a function that returns the final state as JSON, so there is no native binding to keep
//! in sync.
//!
//! What scripts see:
//! - Values are text: `set` stores strings as they are, objects / arrays as `JSON.stringify`, other
//!   values as `String(x)`. `get` returns a string, or `undefined` when the key is missing.
//! - `pm.variables.get` looks in the variables, then the environment, then the globals.
//! - `pm.request.url` is an object (`toString()`, `update(s)`, `getHost()`, `getPath()`,
//!   `getQueryString()`, `query.get/add/upsert/remove/toObject/all`). `pm.request.url + ""` and
//!   assigning a plain string (`pm.request.url = "https://…"`) both work.
//! - `pm.request.body.raw` is the body text (writable; `pm.request.body.update(s)` too).
//! - `setTimeout` runs its callback after the script, without waiting (the delay is ignored).

use rquickjs::{Context, Ctx, Error, Function, Runtime, Value};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// When a script runs: before the request is sent, or after its response arrived.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Pre,
    Post,
}

/// The request as a script sees it (and, in [`Phase::Pre`], may change it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The response given to a [`Phase::Post`] script.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub time_ms: u64,
}

/// Everything a script starts from.
#[derive(Clone, Debug, Default)]
pub struct ScriptInput {
    pub request: ScriptRequest,
    /// Some in [`Phase::Post`].
    pub response: Option<ScriptResponse>,
    /// The active environment's variables.
    pub environment: Vec<(String, String)>,
    /// Runtime / collection variables.
    pub variables: Vec<(String, String)>,
    pub globals: Vec<(String, String)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Log,
    Info,
    Warn,
    Error,
}

/// One `pm.test` (or legacy `tests["name"] = bool`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    /// Why it failed (`AssertionError: expected 1 to equal 2`).
    pub error: Option<String>,
}

/// What a script left behind.
#[derive(Clone, Debug, Default)]
pub struct ScriptOutput {
    /// As the pre-request script left it (unchanged in [`Phase::Post`]).
    pub request: ScriptRequest,
    /// Final values: set / unset applied, order kept, new keys appended.
    pub environment: Vec<(String, String)>,
    pub variables: Vec<(String, String)>,
    pub globals: Vec<(String, String)>,
    pub logs: Vec<(LogLevel, String)>,
    pub tests: Vec<TestResult>,
    /// Uncaught exception, syntax error or time-out, with the line number when known.
    pub error: Option<String>,
    /// `pm.execution.skipRequest()` was called (in [`Phase::Pre`]).
    pub skip_request: bool,
}

/// How much a script may use.
/// What the scripts of one request did, shown with its response (Tests, Console).
#[derive(Clone, Debug, Default)]
pub struct ScriptReport {
    pub logs: Vec<(LogLevel, String)>,
    pub tests: Vec<TestResult>,
    /// `Pre-request script: …` / `Post-response script: …`.
    pub errors: Vec<String>,
}

impl ScriptReport {
    pub fn is_empty(&self) -> bool {
        self.logs.is_empty() && self.tests.is_empty() && self.errors.is_empty()
    }
}

/// Runs a request's pre-request script on what is about to be sent, with the current variables;
/// what it changes in the variables is applied (`env::apply_script`). Returns the request as the
/// script left it, whether to skip the call, and its report.
pub fn before(script: &str, req: ScriptRequest, report: &mut ScriptReport) -> (ScriptRequest, bool) {
    if script.trim().is_empty() {
        return (req, false);
    }
    let input = ScriptInput { request: req, response: None, environment: crate::env::current(), variables: crate::env::runtime(), globals: crate::env::globals() };
    let out = run(script, Phase::Pre, &input, &Limits::default());
    crate::env::apply_script(&input.environment, &out.environment, out.variables.clone(), out.globals.clone());
    report.logs.extend(out.logs);
    report.tests.extend(out.tests);
    if let Some(e) = out.error {
        report.errors.push(format!("Pre-request script: {e}"));
    }
    (out.request, out.skip_request)
}

/// Runs a request's post-response script on its answer.
pub fn after(script: &str, req: ScriptRequest, resp: ScriptResponse, report: &mut ScriptReport) {
    if script.trim().is_empty() {
        return;
    }
    let input = ScriptInput { request: req, response: Some(resp), environment: crate::env::current(), variables: crate::env::runtime(), globals: crate::env::globals() };
    let out = run(script, Phase::Post, &input, &Limits::default());
    crate::env::apply_script(&input.environment, &out.environment, out.variables.clone(), out.globals.clone());
    report.logs.extend(out.logs);
    report.tests.extend(out.tests);
    if let Some(e) = out.error {
        report.errors.push(format!("Post-response script: {e}"));
    }
}

#[derive(Clone, Debug)]
pub struct Limits {
    pub timeout_ms: u64,
    pub memory_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { timeout_ms: 2000, memory_bytes: 64 * 1024 * 1024 }
    }
}

/// File name of the user's script in stack traces (line numbers are looked up after it).
const SCRIPT_FILE: &str = "script";

/// Runs `script` in a fresh QuickJS runtime. Blocking; safe from any thread.
pub fn run(script: &str, phase: Phase, input: &ScriptInput, limits: &Limits) -> ScriptOutput {
    let unchanged = ScriptOutput {
        request: input.request.clone(),
        environment: input.environment.clone(),
        variables: input.variables.clone(),
        globals: input.globals.clone(),
        ..Default::default()
    };
    if script.trim().is_empty() {
        return unchanged;
    }
    match run_js(script, phase, input, limits) {
        Ok(out) => out,
        Err(error) => ScriptOutput { error: Some(error), ..unchanged },
    }
}

/// The state the prelude's `finish` returns, as JSON.
#[derive(Deserialize)]
struct RawOutput {
    request: RawRequest,
    environment: Vec<(String, String)>,
    variables: Vec<(String, String)>,
    globals: Vec<(String, String)>,
    logs: Vec<(String, String)>,
    tests: Vec<RawTest>,
    skip: bool,
}

#[derive(Deserialize)]
struct RawRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Deserialize)]
struct RawTest {
    name: String,
    passed: bool,
    error: Option<String>,
}

/// The input handed to the prelude.
fn input_json(phase: Phase, input: &ScriptInput) -> String {
    let req = &input.request;
    serde_json::json!({
        "phase": if phase == Phase::Pre { "pre" } else { "post" },
        "request": { "method": req.method, "url": req.url, "headers": req.headers, "body": req.body },
        "response": input.response.as_ref().filter(|_| phase == Phase::Post).map(|r| serde_json::json!({
            "code": r.status, "status": r.status_text, "headers": r.headers, "body": r.body, "time": r.time_ms,
        })),
        "environment": input.environment,
        "variables": input.variables,
        "globals": input.globals,
    })
    .to_string()
}

/// Err = the script couldn't run or its state couldn't be read back (the input is kept then).
fn run_js(script: &str, phase: Phase, input: &ScriptInput, limits: &Limits) -> Result<ScriptOutput, String> {
    let rt = Runtime::new().map_err(|e| format!("script engine: {e}"))?;
    rt.set_memory_limit(limits.memory_bytes);
    rt.set_max_stack_size(512 * 1024);
    let armed = Arc::new(AtomicBool::new(true));
    let fired = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + Duration::from_millis(limits.timeout_ms);
    {
        let (armed, fired) = (armed.clone(), fired.clone());
        rt.set_interrupt_handler(Some(Box::new(move || {
            let stop = armed.load(Ordering::Relaxed) && Instant::now() >= deadline;
            if stop {
                fired.store(true, Ordering::Relaxed);
            }
            stop
        })));
    }
    let ctx = Context::full(&rt).map_err(|e| format!("script engine: {e}"))?;
    let timed_out = || format!("timed out after {} ms", limits.timeout_ms);
    ctx.with(|ctx| {
        let finish: Function = (|| {
            let setup: Function = ctx.eval_with_options(PRELUDE, options("prelude"))?;
            let input = ctx.json_parse(input_json(phase, input))?;
            setup.call::<_, Function>((input,))
        })()
        .map_err(|e| if fired.load(Ordering::Relaxed) { timed_out() } else { format!("script prelude: {}", describe(&ctx, e)) })?;

        let mut error = match ctx.eval_with_options::<(), _>(script, options(SCRIPT_FILE)) {
            Ok(()) => None,
            Err(_) if fired.load(Ordering::Relaxed) => Some(timed_out()),
            Err(e) => Some(describe(&ctx, e)),
        };
        // Promise callbacks (async tests, `setTimeout`), still under the time limit.
        if error.is_none() {
            while ctx.execute_pending_job() {
                if fired.load(Ordering::Relaxed) {
                    error = Some(timed_out());
                    break;
                }
            }
        }
        armed.store(false, Ordering::Relaxed);

        let json: String = finish.call(()).map_err(|e| format!("script state: {}", describe(&ctx, e)))?;
        let raw: RawOutput = serde_json::from_str(&json).map_err(|e| format!("script state: {e}"))?;
        let level = |l: &str| match l {
            "info" => LogLevel::Info,
            "warn" => LogLevel::Warn,
            "error" => LogLevel::Error,
            _ => LogLevel::Log,
        };
        Ok(ScriptOutput {
            request: if phase == Phase::Pre {
                ScriptRequest { method: raw.request.method, url: raw.request.url, headers: raw.request.headers, body: raw.request.body }
            } else {
                input.request.clone()
            },
            environment: raw.environment,
            variables: raw.variables,
            globals: raw.globals,
            logs: raw.logs.into_iter().map(|(l, m)| (level(&l), m)).collect(),
            tests: raw.tests.into_iter().map(|t| TestResult { name: t.name, passed: t.passed, error: t.error }).collect(),
            error,
            skip_request: phase == Phase::Pre && raw.skip,
        })
    })
}

/// Sloppy-mode global code (like Postman's sandbox), named `file` in stack traces.
fn options(file: &str) -> rquickjs::context::EvalOptions {
    let mut o = rquickjs::context::EvalOptions::default();
    o.strict = false;
    o.filename = Some(file.to_string());
    o
}

/// `Name: message (line N)` for a JS exception, the error itself otherwise.
fn describe(ctx: &Ctx, e: Error) -> String {
    if !matches!(e, Error::Exception) {
        return e.to_string();
    }
    let value: Value = ctx.catch();
    let Some(obj) = value.as_object().filter(|o| o.is_error()) else {
        let text = value.get::<rquickjs::convert::Coerced<String>>().map(|c| c.0).unwrap_or_else(|_| "exception".into());
        return format!("Uncaught {text}");
    };
    let get = |k: &str| obj.get::<_, Option<rquickjs::convert::Coerced<String>>>(k).ok().flatten().map(|c| c.0).unwrap_or_default();
    let (name, message, stack) = (get("name"), get("message"), get("stack"));
    let mut text = if name.is_empty() { message } else { format!("{name}: {message}") };
    if let Some(line) = script_line(&stack) {
        text.push_str(&format!(" (line {line})"));
    }
    text
}

/// The first `script:<line>` of a QuickJS stack trace.
fn script_line(stack: &str) -> Option<u32> {
    let needle = format!("{SCRIPT_FILE}:");
    let mut rest = stack;
    while let Some(i) = rest.find(&needle) {
        let after = &rest[i + needle.len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        let boundary = i == 0 || !rest[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_');
        if boundary && !digits.is_empty() {
            return digits.parse().ok();
        }
        rest = after;
    }
    None
}

/// The `pm` API. A function of the input that sets the globals and returns `finish`, which gives the
/// final state as JSON.
const PRELUDE: &str = r##"
(function (input) {
  'use strict';
  const G = globalThis;
  const logs = [], tests = [], legacyTests = {};
  let skip = false;

  // ---- values ----
  function str(v) {
    if (typeof v === 'string') return v;
    if (v === undefined) return '';
    if (v !== null && typeof v === 'object') { try { const s = JSON.stringify(v); return s === undefined ? String(v) : s; } catch (e) { return String(v); } }
    return String(v);
  }
  function inspect(v) {
    if (typeof v === 'string') return "'" + v + "'";
    if (v === undefined) return 'undefined';
    if (typeof v === 'function') return '[Function' + (v.name ? ': ' + v.name : '') + ']';
    if (typeof v === 'bigint') return v + 'n';
    if (v === null || typeof v !== 'object') return Object.is(v, -0) ? '-0' : String(v);
    if (v instanceof RegExp) return String(v);
    if (v instanceof Date) return isNaN(v) ? 'Invalid Date' : v.toISOString();
    if (v instanceof Error) return String(v);
    let s;
    try { s = JSON.stringify(v); } catch (e) { s = undefined; }
    if (s === undefined) s = Object.prototype.toString.call(v);
    return s.length > 200 ? s.slice(0, 197) + '...' : s;
  }
  function errText(e) {
    if (e instanceof Error) return (e.name || 'Error') + ': ' + e.message;
    return String(e);
  }
  function deepEqual(a, b, seen) {
    if (a === b) return true;
    if (a !== a && b !== b) return true;
    if (a === null || b === null || typeof a !== 'object' || typeof b !== 'object') return false;
    if (Object.getPrototypeOf(a) !== Object.getPrototypeOf(b)) return false;
    if (a instanceof Date) return a.getTime() === b.getTime();
    if (a instanceof RegExp) return String(a) === String(b);
    seen = seen || [];
    for (const [x, y] of seen) if (x === a && y === b) return true;
    seen.push([a, b]);
    if (a instanceof Map || a instanceof Set) {
      if (a.size !== b.size) return false;
      return deepEqual([...a.entries()], [...b.entries()], seen);
    }
    const ka = Object.keys(a), kb = Object.keys(b);
    if (ka.length !== kb.length) return false;
    for (const k of ka) if (!Object.prototype.hasOwnProperty.call(b, k) || !deepEqual(a[k], b[k], seen)) return false;
    return true;
  }

  // ---- variable scopes ----
  const pairs = list => new Map((list || []).map(p => [String(p[0]), String(p[1])]));
  const envStore = pairs(input.environment), varStore = pairs(input.variables), globStore = pairs(input.globals);
  function lookup(k) {
    k = String(k);
    for (const s of [varStore, envStore, globStore]) if (s.has(k)) return s.get(k);
    return undefined;
  }
  function randomHex(n) { let s = ''; for (let i = 0; i < n; i++) s += Math.floor(Math.random() * 16).toString(16); return s; }
  const dynamic = {
    $guid: () => randomHex(8) + '-' + randomHex(4) + '-4' + randomHex(3) + '-' + '89ab'[Math.floor(Math.random() * 4)] + randomHex(3) + '-' + randomHex(12),
    $timestamp: () => String(Math.floor(Date.now() / 1000)),
    $isoTimestamp: () => new Date().toISOString(),
    $randomInt: () => String(Math.floor(Math.random() * 1001)),
  };
  dynamic.$randomUUID = dynamic.$guid;
  function replaceIn(s, get) {
    if (typeof s !== 'string') return s;
    return s.replace(/\{\{\s*([^{}]+?)\s*\}\}/g, (m, name) => {
      const v = get(name);
      if (v !== undefined) return v;
      return Object.prototype.hasOwnProperty.call(dynamic, name) ? dynamic[name]() : m;
    });
  }
  function scope(store, get) {
    get = get || (k => store.get(String(k)));
    return {
      get: k => get(k),
      set: (k, v) => { store.set(String(k), str(v)); },
      unset: k => { store.delete(String(k)); },
      has: k => get(k) !== undefined,
      clear: () => store.clear(),
      toObject: () => Object.fromEntries(store),
      replaceIn: s => replaceIn(s, get),
    };
  }
  const environment = scope(envStore);
  const globals = scope(globStore);
  const variables = scope(varStore, lookup);
  variables.toObject = () => Object.assign(Object.fromEntries(globStore), Object.fromEntries(envStore), Object.fromEntries(varStore));
  environment.name = '';

  // ---- headers / url / body ----
  function header(h, value) {
    if (typeof h === 'string') {
      if (value !== undefined) return { key: h, value: str(value) };
      const i = h.indexOf(':');
      return i < 0 ? { key: h.trim(), value: '' } : { key: h.slice(0, i).trim(), value: h.slice(i + 1).trim() };
    }
    if (h && typeof h === 'object' && h.key !== undefined) return h.disabled ? null : { key: String(h.key), value: str(h.value) };
    return null;
  }
  class HeaderList {
    constructor(list) { this._h = (list || []).map(p => ({ key: String(p[0]), value: String(p[1]) })); }
    _index(name) { name = String(name).toLowerCase(); return this._h.findIndex(h => h.key.toLowerCase() === name); }
    get(name) { const i = this._index(name); return i < 0 ? undefined : this._h[i].value; }
    has(name, value) { const v = this.get(name); return v !== undefined && (value === undefined || v === String(value)); }
    add(h, value) { h = header(h, value); if (h) this._h.push(h); }
    append(h, value) { this.add(h, value); }
    upsert(h, value) { h = header(h, value); if (!h) return; const i = this._index(h.key); if (i < 0) this._h.push(h); else this._h[i].value = h.value; }
    remove(name) {
      if (typeof name === 'function') { this._h = this._h.filter(h => !name(h)); return; }
      if (name && typeof name === 'object') name = name.key;
      name = String(name).toLowerCase();
      this._h = this._h.filter(h => h.key.toLowerCase() !== name);
    }
    clear() { this._h = []; }
    count() { return this._h.length; }
    all() { return this._h.map(h => ({ key: h.key, value: h.value })); }
    each(fn) { this.all().forEach(fn); }
    toObject() { const o = {}; for (const h of this._h) o[h.key] = h.value; return o; }
    toJSON() { return this.all(); }
    toString() { return this._h.map(h => h.key + ': ' + h.value).join('\n'); }
  }
  const split = raw => {
    const m = /^([a-zA-Z][a-zA-Z0-9+.-]*:\/\/)?([^/?#]*)([^?#]*)(\?[^#]*)?(#.*)?$/.exec(raw) || [];
    return { scheme: m[1] || '', host: m[2] || '', path: m[3] || '', query: (m[4] || '').slice(1), hash: m[5] || '' };
  };
  class QueryList {
    constructor(url) { this._url = url; }
    _items() { const q = split(this._url._raw).query; return q ? q.split('&').map(p => { const i = p.indexOf('='); return i < 0 ? { key: p, value: '' } : { key: p.slice(0, i), value: p.slice(i + 1) }; }) : []; }
    _write(items) { const p = split(this._url._raw); const q = items.map(i => i.value === '' ? i.key : i.key + '=' + i.value).join('&'); this._url._raw = p.scheme + p.host + p.path + (q ? '?' + q : '') + p.hash; }
    _norm(q, value) { if (typeof q === 'string') { if (value !== undefined) return { key: q, value: str(value) }; const i = q.indexOf('='); return i < 0 ? { key: q, value: '' } : { key: q.slice(0, i), value: q.slice(i + 1) }; } return q && q.key !== undefined ? { key: String(q.key), value: q.value == null ? '' : str(q.value) } : null; }
    get(key) { const i = this._items().find(i => i.key === String(key)); return i ? i.value : undefined; }
    has(key) { return this.get(key) !== undefined; }
    add(q, value) { q = this._norm(q, value); if (q) this._write(this._items().concat([q])); }
    upsert(q, value) { q = this._norm(q, value); if (!q) return; const items = this._items(); const i = items.findIndex(x => x.key === q.key); if (i < 0) items.push(q); else items[i] = q; this._write(items); }
    remove(key) { if (key && typeof key === 'object') key = key.key; this._write(this._items().filter(i => i.key !== String(key))); }
    clear() { this._write([]); }
    count() { return this._items().length; }
    all() { return this._items(); }
    each(fn) { this._items().forEach(fn); }
    toObject() { const o = {}; for (const i of this._items()) o[i.key] = i.value; return o; }
    toString() { return split(this._url._raw).query; }
  }
  class Url {
    constructor(raw) { this._raw = String(raw); this.query = new QueryList(this); }
    toString() { return this._raw; }
    toJSON() { return this._raw; }
    valueOf() { return this._raw; }
    update(raw) { this._raw = String(raw); }
    getHost() { return split(this._raw).host.replace(/:\d+$/, ''); }
    getPath() { return split(this._raw).path || '/'; }
    getQueryString() { return split(this._raw).query; }
    getPathWithQuery() { const q = this.getQueryString(); return this.getPath() + (q ? '?' + q : ''); }
    getRaw() { return this._raw; }
  }
  const reqIn = input.request;
  const request = {
    method: String(reqIn.method),
    url: new Url(reqIn.url),
    headers: new HeaderList(reqIn.headers),
    body: {
      mode: 'raw',
      raw: String(reqIn.body),
      update(v) { this.raw = typeof v === 'string' ? v : (v && typeof v === 'object' && v.raw !== undefined ? str(v.raw) : str(v)); },
      isEmpty() { return str(this.raw) === ''; },
      toString() { return str(this.raw); },
    },
    addHeader(h, v) { this.headers.add(h, v); },
    upsertHeader(h, v) { this.headers.upsert(h, v); },
    removeHeader(name) { this.headers.remove(name); },
  };

  // ---- assertions ----
  class AssertionError extends Error { constructor(message) { super(message); this.name = 'AssertionError'; } }
  const typeName = v => v === null ? 'null' : Array.isArray(v) ? 'array' : v instanceof Date ? 'date' : v instanceof RegExp ? 'regexp' : typeof v;
  const article = t => (/^[aeiou]/i.test(t) ? 'an ' : 'a ') + t;
  function Assertion(obj, message) { this._obj = obj; this._neg = false; this._deep = false; this._any = false; this._msg = message; }
  const A = Assertion.prototype;
  A._check = function (ok, what, actual) {
    if (this._neg ? !ok : ok) return this;
    const text = 'expected ' + inspect(this._obj) + ' to ' + (this._neg ? 'not ' : '') + what + (actual !== undefined && !this._neg ? ' but got ' + actual : '');
    throw new AssertionError(this._msg ? this._msg + ': ' + text : text);
  };
  A._eq = function (a, b) { return this._deep ? deepEqual(a, b) : a === b; };
  A._num = function (what) {
    const o = this._obj;
    if (typeof o !== 'number' && !(o instanceof Date)) throw new AssertionError('expected ' + inspect(o) + ' to be a number or a date (for ' + what + ')');
  };
  for (const w of ['to', 'be', 'been', 'is', 'that', 'which', 'and', 'has', 'have', 'with', 'at', 'of', 'same', 'but', 'does', 'still', 'also', 'own', 'all', 'nested', 'ordered'])
    Object.defineProperty(A, w, { get() { return this; } });
  Object.defineProperty(A, 'not', { get() { this._neg = !this._neg; return this; } });
  Object.defineProperty(A, 'deep', { get() { this._deep = true; return this; } });
  Object.defineProperty(A, 'any', { get() { this._any = true; return this; } });
  const getters = {
    true: function () { return this._check(this._obj === true, 'be true'); },
    false: function () { return this._check(this._obj === false, 'be false'); },
    null: function () { return this._check(this._obj === null, 'be null'); },
    undefined: function () { return this._check(this._obj === undefined, 'be undefined'); },
    NaN: function () { return this._check(typeof this._obj === 'number' && this._obj !== this._obj, 'be NaN'); },
    ok: function () { return this._check(!!this._obj, 'be truthy'); },
    exist: function () { return this._check(this._obj != null, 'exist'); },
    finite: function () { return this._check(Number.isFinite(this._obj), 'be a finite number'); },
    empty: function () {
      const o = this._obj;
      let empty;
      if (typeof o === 'string' || Array.isArray(o)) empty = o.length === 0;
      else if (o instanceof Map || o instanceof Set) empty = o.size === 0;
      else if (o !== null && typeof o === 'object') empty = Object.keys(o).length === 0;
      else throw new AssertionError('.empty was passed non-string primitive ' + inspect(o));
      return this._check(empty, 'be empty');
    },
  };
  for (const [k, f] of Object.entries(getters)) Object.defineProperty(A, k, { get: f });
  A.equal = A.equals = A.eq = function (x) { return this._check(this._eq(this._obj, x), (this._deep ? 'deeply equal ' : 'equal ') + inspect(x)); };
  A.eql = A.eqls = function (x) { return this._check(deepEqual(this._obj, x), 'deeply equal ' + inspect(x)); };
  A.above = A.gt = A.greaterThan = function (n) { this._num('above'); return this._check(this._obj > n, 'be above ' + inspect(n)); };
  A.below = A.lt = A.lessThan = function (n) { this._num('below'); return this._check(this._obj < n, 'be below ' + inspect(n)); };
  A.least = A.gte = A.greaterThanOrEqual = function (n) { this._num('least'); return this._check(this._obj >= n, 'be at least ' + inspect(n)); };
  A.most = A.lte = A.lessThanOrEqual = function (n) { this._num('most'); return this._check(this._obj <= n, 'be at most ' + inspect(n)); };
  A.within = function (a, b) { this._num('within'); return this._check(this._obj >= a && this._obj <= b, 'be within ' + inspect(a) + '..' + inspect(b)); };
  A.closeTo = A.approximately = function (x, delta) { this._num('closeTo'); return this._check(Math.abs(this._obj - x) <= delta, 'be close to ' + inspect(x) + ' +/- ' + inspect(delta)); };
  A.include = A.includes = A.contain = A.contains = function (x) {
    const o = this._obj;
    let ok;
    if (typeof o === 'string') ok = o.includes(String(x));
    else if (Array.isArray(o)) ok = o.some(v => this._eq(v, x));
    else if (o instanceof Set) ok = [...o].some(v => this._eq(v, x));
    else if (o instanceof Map) ok = [...o.values()].some(v => this._eq(v, x));
    else if (o !== null && typeof o === 'object' && x !== null && typeof x === 'object')
      ok = Object.keys(x).every(k => k in o && this._eq(o[k], x[k]));
    else ok = false;
    return this._check(ok, 'include ' + inspect(x));
  };
  A.property = function (name, value) {
    const o = this._obj;
    const has = o != null && name in Object(o);
    if (arguments.length > 1) {
      const ok = has && this._eq(o[name], value);
      return this._check(ok, 'have ' + (this._deep ? 'deep ' : '') + 'property ' + inspect(name) + ' of ' + inspect(value), has ? inspect(o[name]) : undefined);
    }
    this._check(has, 'have property ' + inspect(name));
    if (!this._neg) this._obj = o[name];
    return this;
  };
  A.ownProperty = A.haveOwnProperty = function (name) { return this._check(this._obj != null && Object.prototype.hasOwnProperty.call(this._obj, name), 'have own property ' + inspect(name)); };
  A.lengthOf = A.length = function (n) {
    const o = this._obj;
    const len = o == null ? undefined : (o instanceof Map || o instanceof Set) ? o.size : o.length;
    return this._check(len === n, 'have a length of ' + inspect(n), inspect(len));
  };
  A.match = A.matches = function (re) { return this._check(re instanceof RegExp ? re.test(String(this._obj)) : String(this._obj).includes(String(re)), 'match ' + inspect(re)); };
  A.string = function (s) { return this._check(typeof this._obj === 'string' && this._obj.includes(s), 'contain ' + inspect(s)); };
  A.oneOf = function (list) { return this._check(Array.isArray(list) && list.some(v => this._eq(this._obj, v)), 'be one of ' + inspect(list)); };
  A.a = A.an = function (type) { const t = String(type).toLowerCase(); return this._check(typeName(this._obj) === t, 'be ' + article(t), typeName(this._obj)); };
  A.instanceOf = A.instanceof = function (C) { return this._check(this._obj instanceof C, 'be an instance of ' + (C && C.name || inspect(C))); };
  A.satisfy = A.satisfies = function (fn) { return this._check(!!fn(this._obj), 'satisfy ' + inspect(fn)); };
  A.members = function (list) {
    const o = this._obj;
    const ok = Array.isArray(o) && Array.isArray(list) && o.length === list.length && list.every(x => o.some(v => this._eq(v, x))) && o.every(v => list.some(x => this._eq(v, x)));
    return this._check(ok, 'have the same members as ' + inspect(list));
  };
  A.keys = A.key = function (...keys) {
    if (keys.length === 1 && Array.isArray(keys[0])) keys = keys[0];
    else if (keys.length === 1 && keys[0] !== null && typeof keys[0] === 'object') keys = Object.keys(keys[0]);
    const o = this._obj;
    const actual = o instanceof Map ? [...o.keys()] : o != null ? Object.keys(o) : [];
    const ok = this._any ? keys.some(k => actual.includes(k)) : keys.length === actual.length && keys.every(k => actual.includes(k));
    return this._check(ok, 'have ' + (this._any ? 'any of ' : '') + 'keys ' + keys.map(inspect).join(', '));
  };
  A.throw = A.throws = A.Throw = function (expected) {
    let thrown = false, err;
    try { this._obj(); } catch (e) { thrown = true; err = e; }
    let ok = thrown, what = 'throw an error';
    if (thrown && expected !== undefined) {
      const msg = err instanceof Error ? err.message : String(err);
      if (expected instanceof RegExp) { ok = expected.test(msg); what = 'throw an error matching ' + inspect(expected); }
      else if (typeof expected === 'string') { ok = msg.includes(expected); what = 'throw an error including ' + inspect(expected); }
      else if (typeof expected === 'function') { ok = err instanceof expected; what = 'throw ' + (expected.name || 'the expected error'); }
    }
    return this._check(ok, what);
  };
  function expect(value, message) { return new Assertion(value, message); }
  expect.fail = message => { throw new AssertionError(message === undefined ? 'expect.fail()' : String(message)); };

  function test(name, fn) {
    if (typeof fn !== 'function') return;
    const t = { name: String(name), passed: true, error: null, pending: false };
    tests.push(t);
    try {
      const r = fn();
      if (r && typeof r.then === 'function') {
        t.pending = true;
        r.then(() => { t.pending = false; }, e => { t.pending = false; t.passed = false; t.error = errText(e); });
      }
    } catch (e) {
      t.passed = false;
      t.error = errText(e);
    }
  }
  test.skip = function () {};

  // ---- response ----
  let response;
  const resIn = input.phase === 'post' ? input.response : null;
  if (resIn) {
    const body = String(resIn.body);
    response = {
      code: resIn.code,
      status: String(resIn.status),
      responseTime: resIn.time,
      responseSize: body.length,
      headers: new HeaderList(resIn.headers),
      text: () => body,
      json: () => {
        try { return JSON.parse(body); }
        catch (e) { const err = new Error('pm.response.json(): the response body is not valid JSON (' + e.message + ')'); err.name = 'JSONError'; throw err; }
      },
      reason: () => String(resIn.status),
    };
    const classes = { info: c => c >= 100 && c < 200, success: c => c >= 200 && c < 300, redirection: c => c >= 300 && c < 400,
      clientError: c => c >= 400 && c < 500, serverError: c => c >= 500 && c < 600, error: c => c >= 400 && c < 600,
      ok: c => c === 200, accepted: c => c === 202, withoutContent: c => c === 204, badRequest: c => c === 400, unauthorized: c => c === 401,
      forbidden: c => c === 403, notFound: c => c === 404, rateLimited: c => c === 429 };
    const makeTo = neg => {
      const t = {};
      const check = (ok, what) => {
        if (neg ? !ok : ok) return t;
        throw new AssertionError('expected response to ' + (neg ? 'not ' : '') + what + ' (status ' + response.code + ')');
      };
      for (const w of ['to', 'be', 'have', 'a', 'an', 'that', 'and', 'with', 'is', 'has'])
        Object.defineProperty(t, w, { get() { return t; } });
      Object.defineProperty(t, 'not', { get() { return makeTo(!neg); } });
      for (const [k, f] of Object.entries(classes)) Object.defineProperty(t, k, { get() { return check(f(response.code), 'be ' + k); } });
      Object.defineProperty(t, 'json', { get() { let ok = true; try { JSON.parse(body); } catch (e) { ok = false; } return check(ok, 'be json'); } });
      t.status = s => typeof s === 'number' ? check(response.code === s, 'have status code ' + s) : check(response.status === String(s), 'have status ' + inspect(String(s)));
      t.header = (k, v) => v === undefined ? check(response.headers.has(k), 'have header ' + inspect(k)) : check(response.headers.get(k) === String(v), 'have header ' + inspect(k) + ' with value ' + inspect(String(v)));
      t.body = s => s === undefined ? check(body !== '', 'have a body') : check(s instanceof RegExp ? s.test(body) : body === String(s), 'have body ' + inspect(s));
      t.jsonBody = (path, v) => {
        let j; try { j = JSON.parse(body); } catch (e) { return check(false, 'have a json body'); }
        if (path === undefined) return t;
        if (typeof path === 'object') return check(deepEqual(j, path), 'have json body ' + inspect(path));
        let cur = j; for (const p of String(path).split('.')) cur = cur == null ? undefined : cur[p];
        return check(v === undefined ? cur !== undefined : deepEqual(cur, v), 'have json body property ' + inspect(path) + (v === undefined ? '' : ' of ' + inspect(v)));
      };
      return t;
    };
    response.to = makeTo(false);
  }

  // ---- globals ----
  const fmt = v => typeof v === 'string' ? v : v instanceof Error ? String(v) : (v !== null && typeof v === 'object') ? str(v) : typeof v === 'function' ? inspect(v) : String(v);
  const logger = level => (...args) => { logs.push([level, args.map(fmt).join(' ')]); };
  const noNetwork = what => () => { throw new Error(what + ' is not available: scripts have no network or file access'); };
  const pm = {
    environment, variables, globals, collectionVariables: variables,
    request, response, test, expect,
    info: { eventName: input.phase === 'pre' ? 'prerequest' : 'test', iteration: 0, iterationCount: 1, requestName: '', requestId: '' },
    execution: { skipRequest: () => { skip = true; }, setNextRequest: () => {}, location: [] },
    sendRequest: noNetwork('pm.sendRequest'),
    iterationData: scope(new Map()),
  };
  G.pm = pm;
  G.console = { log: logger('log'), info: logger('info'), warn: logger('warn'), error: logger('error'), debug: logger('log') };
  G.postman = {
    setEnvironmentVariable: (k, v) => environment.set(k, v),
    getEnvironmentVariable: k => environment.get(k),
    clearEnvironmentVariable: k => environment.unset(k),
    setGlobalVariable: (k, v) => globals.set(k, v),
    getGlobalVariable: k => globals.get(k),
    clearGlobalVariable: k => globals.unset(k),
    getVariable: k => variables.get(k),
    setNextRequest: () => {},
  };
  G.tests = legacyTests;
  G.require = noNetwork('require');
  G.fetch = noNetwork('fetch');
  G.XMLHttpRequest = undefined;
  let timers = 0;
  const cancelled = new Set();
  G.setTimeout = (fn, ms, ...args) => { const id = ++timers; Promise.resolve().then(() => { if (!cancelled.has(id) && typeof fn === 'function') fn(...args); }); return id; };
  G.clearTimeout = id => { cancelled.add(id); };
  G.setInterval = () => { throw new Error('setInterval is not available in scripts'); };
  G.clearInterval = () => {};
  if (resIn) {
    G.responseBody = String(resIn.body);
    G.responseCode = { code: resIn.code, name: String(resIn.status), detail: String(resIn.status) };
    G.responseTime = resIn.time;
    G.responseHeaders = response.headers.toObject();
  }

  return function finish() {
    const out = tests.map(t => ({
      name: t.name,
      passed: t.passed && !t.pending,
      error: t.pending ? 'the test did not finish (a promise never settled)' : t.error,
    }));
    for (const [k, v] of Object.entries(legacyTests)) out.push({ name: k, passed: !!v, error: v ? null : 'test failed' });
    const r = pm.request || request;
    const headers = r.headers instanceof HeaderList ? r.headers.all().map(h => [h.key, h.value]) : request.headers.all().map(h => [h.key, h.value]);
    const b = r.body;
    const body = b !== null && typeof b === 'object' ? (b.raw === undefined ? '' : str(b.raw)) : str(b);
    return JSON.stringify({
      request: { method: str(r.method), url: str(r.url === undefined ? '' : String(r.url)), headers, body },
      environment: [...envStore], variables: [...varStore], globals: [...globStore],
      logs, tests: out, skip,
    });
  };
})
"##;

#[cfg(test)]
mod tests {
    use super::*;

    fn kv(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn pre_input() -> ScriptInput {
        ScriptInput {
            request: ScriptRequest {
                method: "GET".into(),
                url: "https://api.example.com/v1/users?page=1".into(),
                headers: kv(&[("Accept", "application/json")]),
                body: String::new(),
            },
            environment: kv(&[("host", "api.example.com"), ("token", "abc")]),
            variables: kv(&[("user", "jdoe")]),
            globals: kv(&[("g", "1")]),
            ..Default::default()
        }
    }

    fn post_input(status: u16, body: &str) -> ScriptInput {
        ScriptInput {
            response: Some(ScriptResponse {
                status,
                status_text: if status == 200 { "OK".into() } else { "Not Found".into() },
                headers: kv(&[("Content-Type", "application/json"), ("X-Total", "40")]),
                body: body.into(),
                time_ms: 123,
            }),
            ..pre_input()
        }
    }

    fn pre(script: &str) -> ScriptOutput {
        run(script, Phase::Pre, &pre_input(), &Limits::default())
    }

    fn post(script: &str) -> ScriptOutput {
        run(script, Phase::Post, &post_input(200, r#"{"id": 7, "login": "jdoe", "tags": ["a", "b"], "nested": {"x": [1, {"y": 2}]}}"#), &Limits::default())
    }

    fn ok(out: &ScriptOutput) {
        assert_eq!(out.error, None, "logs: {:?}", out.logs);
    }

    fn messages(out: &ScriptOutput) -> Vec<String> {
        out.logs.iter().map(|(_, m)| m.clone()).collect()
    }

    #[test]
    fn empty_script_returns_input() {
        let input = pre_input();
        let t = Instant::now();
        let out = run("  \n\t ", Phase::Pre, &input, &Limits::default());
        assert!(t.elapsed() < Duration::from_millis(5));
        assert_eq!(out.request, input.request);
        assert_eq!(out.environment, input.environment);
        assert_eq!(out.variables, input.variables);
        assert_eq!(out.globals, input.globals);
        assert!(out.logs.is_empty() && out.tests.is_empty() && out.error.is_none() && !out.skip_request);
    }

    #[test]
    fn environment_set_get_unset() {
        let out = pre("pm.environment.set('token', 'new'); pm.environment.set('added', 'x'); pm.environment.unset('host');
             console.log(pm.environment.get('token'), pm.environment.has('host'), pm.environment.get('missing'));
             pm.globals.set('g2', 'two'); pm.globals.unset('g');
             postman.setEnvironmentVariable('legacy', 'L'); console.log(postman.getEnvironmentVariable('legacy'));");
        ok(&out);
        assert_eq!(out.environment, kv(&[("token", "new"), ("added", "x"), ("legacy", "L")]));
        assert_eq!(out.globals, kv(&[("g2", "two")]));
        assert_eq!(messages(&out), ["new false undefined", "L"]);
    }

    #[test]
    fn variables_fall_back_and_alias() {
        let out = pre("console.log(pm.variables.get('user'), pm.variables.get('token'), pm.variables.get('g'), pm.variables.has('g'));
             pm.collectionVariables.set('c', 3); pm.variables.set('user', 'asmith');
             console.log(JSON.stringify(pm.variables.toObject()));");
        ok(&out);
        assert_eq!(out.logs[0].1, "jdoe abc 1 true");
        assert_eq!(out.variables, kv(&[("user", "asmith"), ("c", "3")]));
        assert_eq!(out.logs[1].1, r#"{"g":"1","host":"api.example.com","token":"abc","user":"asmith","c":"3"}"#);
    }

    #[test]
    fn non_string_values_are_stored_as_text() {
        let out = pre("pm.environment.set('n', 17); pm.environment.set('b', true); pm.environment.set('o', {a: [1, 'x']}); pm.environment.set('z', null);");
        ok(&out);
        assert_eq!(&out.environment[2..], kv(&[("n", "17"), ("b", "true"), ("o", r#"{"a":[1,"x"]}"#), ("z", "null")]).as_slice());
    }

    #[test]
    fn replace_in() {
        let out = pre("console.log(pm.variables.replaceIn('https://{{host}}/v1/users/{{ user }}?t={{token}}&u={{unknown}}'));
                       console.log(pm.variables.replaceIn('{{$timestamp}}').length >= 10);");
        ok(&out);
        assert_eq!(messages(&out), ["https://api.example.com/v1/users/jdoe?t=abc&u={{unknown}}", "true"]);
    }

    #[test]
    fn pre_changes_the_request() {
        let out = pre("pm.request.headers.add({key: 'X-One', value: '1'});
             pm.request.headers.upsert({key: 'accept', value: 'text/plain'});
             pm.request.headers.add('X-Two: 2'); pm.request.headers.remove('x-two');
             pm.request.headers.upsert({key: 'Authorization', value: 'Bearer ' + pm.environment.get('token')});
             console.log(pm.request.headers.get('ACCEPT'), JSON.stringify(pm.request.headers.toObject()), pm.request.headers.all().length);
             pm.request.method = 'POST';
             pm.request.body.raw = JSON.stringify({login: pm.variables.get('user')});
             pm.request.url.query.upsert({key: 'page', value: '2'}); pm.request.url.query.add('sort=id');
             console.log(pm.request.url.toString(), pm.request.url.getHost(), pm.request.url.getPath(), String(pm.request.url) === pm.request.url + '');");
        ok(&out);
        assert_eq!(out.logs[0].1, r#"text/plain {"Accept":"text/plain","X-One":"1","Authorization":"Bearer abc"} 3"#);
        assert_eq!(out.logs[1].1, "https://api.example.com/v1/users?page=2&sort=id api.example.com /v1/users true");
        assert_eq!(out.request.method, "POST");
        assert_eq!(out.request.url, "https://api.example.com/v1/users?page=2&sort=id");
        assert_eq!(out.request.headers, kv(&[("Accept", "text/plain"), ("X-One", "1"), ("Authorization", "Bearer abc")]));
        assert_eq!(out.request.body, r#"{"login":"jdoe"}"#);
    }

    #[test]
    fn url_can_be_replaced_by_a_string() {
        let out = pre("pm.request.url = 'https://other.example.com/x'; pm.request.body.update({a: 1});");
        ok(&out);
        assert_eq!(out.request.url, "https://other.example.com/x");
        assert_eq!(out.request.body, r#"{"a":1}"#);
    }

    #[test]
    fn post_keeps_the_request() {
        let out = post("pm.request.method = 'DELETE'; pm.request.headers.add({key: 'X', value: 'y'});");
        ok(&out);
        assert_eq!(out.request, pre_input().request);
        assert!(!out.skip_request);
    }

    #[test]
    fn response_and_tests() {
        let out = post(
            "const j = pm.response.json();
             pm.environment.set('user_id', j.id);
             pm.test('status is 200', () => pm.response.to.have.status(200));
             pm.test('status is 404', function () { pm.response.to.have.status(404); });
             pm.test('login', () => { pm.expect(j.login).to.eql('jdoe'); pm.expect(j.tags).to.include('b').and.have.lengthOf(2); });
             pm.test('headers', () => { pm.expect(pm.response.headers.get('content-type')).to.include('json'); pm.response.to.have.header('X-Total', '40'); });
             pm.test('ok and time', () => { pm.response.to.be.ok; pm.response.to.not.be.error; pm.expect(pm.response.responseTime).to.be.below(1000); });
             pm.test('throws', () => { throw new TypeError('boom'); });
             tests['legacy'] = responseCode.code === 200;
             console.log(pm.response.code, pm.response.status, pm.response.text().length > 10, pm.info.eventName);",
        );
        ok(&out);
        assert_eq!(out.environment.last().unwrap(), &("user_id".to_string(), "7".to_string()));
        let summary: Vec<(&str, bool)> = out.tests.iter().map(|t| (t.name.as_str(), t.passed)).collect();
        assert_eq!(
            summary,
            [("status is 200", true), ("status is 404", false), ("login", true), ("headers", true), ("ok and time", true), ("throws", false), ("legacy", true)]
        );
        assert_eq!(out.tests[1].error.as_deref(), Some("AssertionError: expected response to have status code 404 (status 200)"));
        assert_eq!(out.tests[5].error.as_deref(), Some("TypeError: boom"));
        assert_eq!(messages(&out), ["200 OK true test"]);
    }

    #[test]
    fn json_error_is_clear() {
        let out = run("pm.response.json();", Phase::Post, &post_input(404, "<html>nope</html>"), &Limits::default());
        let err = out.error.unwrap();
        assert!(err.starts_with("JSONError: pm.response.json(): the response body is not valid JSON"), "{err}");
        assert!(err.ends_with("(line 1)"), "{err}");
    }

    #[test]
    fn response_is_undefined_in_pre() {
        let out = pre("console.log(typeof pm.response, pm.info.eventName);");
        ok(&out);
        assert_eq!(messages(&out), ["undefined prerequest"]);
    }

    /// Runs `expr` in a pm.test and returns its error (None = passed).
    fn expect_err(expr: &str) -> Option<String> {
        let out = post(&format!("pm.test('t', () => {{ {expr}; }});"));
        ok(&out);
        assert_eq!(out.tests.len(), 1);
        assert_eq!(out.tests[0].passed, out.tests[0].error.is_none());
        out.tests[0].error.clone()
    }

    #[test]
    fn expect_passes() {
        for e in [
            "pm.expect(1).to.equal(1)",
            "pm.expect('abc').to.be.a('string').and.to.include('b')",
            "pm.expect([1, 2]).to.be.an('array').that.includes(2)",
            "pm.expect({a: 1, b: 2}).to.include({a: 1})",
            "pm.expect({a: {b: [1, {c: 2}]}}).to.eql({a: {b: [1, {c: 2}]}})",
            "pm.expect({a: [1]}).to.deep.equal({a: [1]})",
            "pm.expect([{a: 1}]).to.deep.include({a: 1})",
            "pm.expect(5).to.be.above(3).and.below(10)",
            "pm.expect(5).to.be.at.least(5).and.at.most(5)",
            "pm.expect(5).to.be.gt(4).and.lt(6).and.gte(5).and.lte(5)",
            "pm.expect('jdoe').to.match(/^[a-z]+$/)",
            "pm.expect(2).to.be.oneOf([1, 2, 3])",
            "pm.expect({id: 3}).to.have.property('id', 3)",
            "pm.expect({id: 3}).to.have.property('id').that.is.a('number')",
            "pm.expect([1, 2, 3]).to.have.lengthOf(3)",
            "pm.expect('').to.be.empty",
            "pm.expect({}).to.be.empty",
            "pm.expect(true).to.be.true",
            "pm.expect(false).to.be.false",
            "pm.expect(null).to.be.null",
            "pm.expect(undefined).to.be.undefined",
            "pm.expect(1).to.be.ok",
            "pm.expect(0).to.exist",
            "pm.expect(null).to.be.a('null')",
            "pm.expect(1).to.not.equal(2)",
            "pm.expect([1]).to.not.be.empty",
            "pm.expect({a: 1}).to.not.eql({a: 2})",
            "pm.expect('x').to.not.be.a('number')",
            "pm.expect({a: 1, b: 2}).to.have.keys('a', 'b')",
            "pm.expect({a: 1, b: 2}).to.have.any.keys('a', 'z')",
            "pm.expect([2, 1]).to.have.members([1, 2])",
            "pm.expect(() => { throw new Error('bad thing'); }).to.throw(/bad/)",
            "pm.expect(pm.response.json().nested).to.eql({x: [1, {y: 2}]})",
        ] {
            assert_eq!(expect_err(e), None, "{e}");
        }
    }

    #[test]
    fn expect_failures_have_messages() {
        let cases = [
            ("pm.expect(1).to.equal(2)", "AssertionError: expected 1 to equal 2"),
            ("pm.expect('a').to.equal('b')", "AssertionError: expected 'a' to equal 'b'"),
            ("pm.expect(1).to.not.equal(1)", "AssertionError: expected 1 to not equal 1"),
            ("pm.expect({a: [1, 2]}).to.eql({a: [1, 3]})", r#"AssertionError: expected {"a":[1,2]} to deeply equal {"a":[1,3]}"#),
            ("pm.expect({a: 1}).to.equal({a: 1})", r#"AssertionError: expected {"a":1} to equal {"a":1}"#),
            ("pm.expect(3).to.be.above(5)", "AssertionError: expected 3 to be above 5"),
            ("pm.expect(7).to.be.below(5)", "AssertionError: expected 7 to be below 5"),
            ("pm.expect([1, 2]).to.include(3)", "AssertionError: expected [1,2] to include 3"),
            ("pm.expect('abc').to.include('z')", "AssertionError: expected 'abc' to include 'z'"),
            ("pm.expect({a: 1}).to.include({a: 2})", r#"AssertionError: expected {"a":1} to include {"a":2}"#),
            ("pm.expect({}).to.have.property('id')", "AssertionError: expected {} to have property 'id'"),
            ("pm.expect({id: 1}).to.have.property('id', 2)", r#"AssertionError: expected {"id":1} to have property 'id' of 2 but got 1"#),
            ("pm.expect([1]).to.have.lengthOf(2)", "AssertionError: expected [1] to have a length of 2 but got 1"),
            ("pm.expect('X').to.match(/^[a-z]$/)", "AssertionError: expected 'X' to match /^[a-z]$/"),
            ("pm.expect(4).to.be.oneOf([1, 2])", "AssertionError: expected 4 to be one of [1,2]"),
            ("pm.expect(1).to.be.a('string')", "AssertionError: expected 1 to be a string but got number"),
            ("pm.expect([]).to.be.an('object')", "AssertionError: expected [] to be an object but got array"),
            ("pm.expect(1).to.be.true", "AssertionError: expected 1 to be true"),
            ("pm.expect(null).to.exist", "AssertionError: expected null to exist"),
            ("pm.expect(undefined).to.not.be.undefined", "AssertionError: expected undefined to not be undefined"),
            ("pm.expect([1]).to.be.empty", "AssertionError: expected [1] to be empty"),
            ("pm.expect(0).to.be.ok", "AssertionError: expected 0 to be truthy"),
            ("pm.expect(1, 'custom').to.equal(2)", "AssertionError: custom: expected 1 to equal 2"),
            ("pm.expect('5').to.be.above(3)", "AssertionError: expected '5' to be a number or a date (for above)"),
            ("pm.expect.fail('nope')", "AssertionError: nope"),
        ];
        for (e, msg) in cases {
            assert_eq!(expect_err(e).as_deref(), Some(msg), "{e}");
        }
    }

    #[test]
    fn deep_equality() {
        for (e, pass) in [
            ("pm.expect([1, [2, {a: null}]]).to.eql([1, [2, {a: null}]])", true),
            ("pm.expect({a: 1, b: 2}).to.eql({b: 2, a: 1})", true),
            ("pm.expect({a: 1}).to.eql({a: 1, b: undefined})", false),
            ("pm.expect([1, 2]).to.eql({0: 1, 1: 2})", false),
            ("pm.expect(NaN).to.eql(NaN)", true),
            ("pm.expect(new Date(5)).to.eql(new Date(5))", true),
            ("pm.expect('1').to.eql(1)", false),
            ("const a = {}; a.self = a; const b = {}; b.self = b; pm.expect(a).to.eql(b)", true),
        ] {
            assert_eq!(expect_err(e).is_none(), pass, "{e}");
        }
    }

    #[test]
    fn async_test_is_recorded() {
        let out = post("pm.test('later', async () => { await null; pm.expect(1).to.equal(2); }); setTimeout(() => console.log('timer'), 100);");
        ok(&out);
        assert_eq!(out.tests[0].error.as_deref(), Some("AssertionError: expected 1 to equal 2"));
        assert_eq!(messages(&out), ["timer"]);
    }

    #[test]
    fn timeout_stops_infinite_loops() {
        let limits = Limits { timeout_ms: 100, ..Limits::default() };
        let t = Instant::now();
        let out = run("pm.environment.set('before', 'yes'); console.log('start'); while (true) {}", Phase::Pre, &pre_input(), &limits);
        assert!(t.elapsed() < Duration::from_secs(2));
        assert_eq!(out.error.as_deref(), Some("timed out after 100 ms"));
        assert_eq!(messages(&out), ["start"]);
        assert!(out.environment.contains(&("before".into(), "yes".into())));
        // try / catch can't swallow it, nor can a test.
        let out = run("try { for (;;) {} } catch (e) {} pm.test('t', () => { for (;;) {} });", Phase::Pre, &pre_input(), &limits);
        assert_eq!(out.error.as_deref(), Some("timed out after 100 ms"));
        let out = run("Promise.resolve().then(function spin() { for (;;) {} });", Phase::Pre, &pre_input(), &limits);
        assert_eq!(out.error.as_deref(), Some("timed out after 100 ms"));
    }

    #[test]
    fn memory_limit() {
        let limits = Limits { memory_bytes: 8 * 1024 * 1024, ..Limits::default() };
        let out = run("const a = []; for (;;) a.push('x'.repeat(1000) + a.length);", Phase::Pre, &pre_input(), &limits);
        assert!(out.error.is_some());
    }

    #[test]
    fn syntax_error_has_line() {
        let out = pre("const a = 1;\nconsole.log(a);\nlet = = 3;\n");
        let err = out.error.unwrap();
        assert!(err.starts_with("SyntaxError"), "{err}");
        assert!(err.ends_with("(line 3)"), "{err}");
        assert!(out.logs.is_empty());
    }

    #[test]
    fn runtime_error_has_line_and_keeps_state() {
        let out = pre("pm.environment.set('a', '1');\nconsole.log('one');\nundefinedFunction();\nconsole.log('two');");
        assert_eq!(out.error.as_deref(), Some("ReferenceError: undefinedFunction is not defined (line 3)"));
        assert_eq!(messages(&out), ["one"]);
        assert!(out.environment.contains(&("a".into(), "1".into())));
        let out = pre("throw 'plain';");
        assert_eq!(out.error.as_deref(), Some("Uncaught plain"));
        let out = pre("\n\npm.expect(1).to.equal(2);");
        assert_eq!(out.error.as_deref(), Some("AssertionError: expected 1 to equal 2 (line 3)"));
    }

    #[test]
    fn console_levels_and_formatting() {
        let out = pre("console.log('a', 1, {b: [2]}, [3], null, undefined, true); console.info('i'); console.warn('w'); console.error(new Error('e'));");
        ok(&out);
        assert_eq!(
            out.logs,
            [
                (LogLevel::Log, r#"a 1 {"b":[2]} [3] null undefined true"#.to_string()),
                (LogLevel::Info, "i".into()),
                (LogLevel::Warn, "w".into()),
                (LogLevel::Error, "Error: e".into())
            ]
        );
    }

    #[test]
    fn skip_request() {
        let out = pre("if (!pm.environment.get('nope')) pm.execution.skipRequest(); console.log('still runs');");
        ok(&out);
        assert!(out.skip_request);
        assert_eq!(messages(&out), ["still runs"]);
        assert!(!post("pm.execution.skipRequest();").skip_request);
    }

    #[test]
    fn no_network_or_files() {
        let out = pre("pm.sendRequest('https://example.com', () => {});");
        assert!(out.error.unwrap().contains("not available"));
        let out = pre("require('fs');");
        assert!(out.error.unwrap().contains("not available"));
        let out = pre("console.log(typeof std, typeof os, typeof XMLHttpRequest);");
        assert_eq!(messages(&out), ["undefined undefined undefined"]);
    }

    #[test]
    fn runs_are_independent() {
        let a = pre("globalThis.leak = 1; pm.environment.set('x', '1');");
        ok(&a);
        let b = pre("console.log(typeof leak, pm.environment.get('x'));");
        assert_eq!(messages(&b), ["undefined undefined"]);
    }

    #[test]
    fn runs_on_other_threads() {
        let handles: Vec<_> = (0..4).map(|i| std::thread::spawn(move || pre(&format!("pm.environment.set('i', {i});")))).collect();
        for (i, h) in handles.into_iter().enumerate() {
            let out = h.join().unwrap();
            assert_eq!(out.environment.last().unwrap().1, i.to_string());
        }
    }
}
