//! Runs a flow on its own thread: blocks, requests and pagination, loops (parallel too), sub-flows, resume.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{Map, Value};

use std::collections::{BTreeMap, HashMap};

use crate::docs::ApiDoc;
use crate::model::{self, Prepared, RequestSpec};
use crate::net::{self, Credentials, ResponseData, SharedAuth};

use super::*;

// ==================================================================== runner

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Info,
    Request,
    Response,
    Error,
    Success,
    Var,
    /// Something the user should know although the run goes on (a list cut by the page limit…).
    Warning,
}

pub struct LogLine {
    pub kind: LogKind,
    pub depth: usize,
    pub text: String,
    pub detail: Option<String>,
}

pub enum RunEvent {
    Log(LogLine),
    /// After each call: the API's rate limits as it just told them.
    Rate(Rate),
    /// Where the outermost running "for each" is (`None` when it ends).
    Progress(Option<Progress>),
    /// The assertions of a request block, checked.
    Tests(Vec<crate::assert::Outcome>),
    Finished {
        ok: bool,
        vars: Value,
    },
}

/// Live numbers of a run, from the response headers.
#[derive(Clone, Copy, Default)]
pub struct Rate {
    /// Requests sent so far in this run (lookups and sub-flows included).
    pub requests: u64,
    pub per_second_left: Option<u64>,
    /// Requests per second the API allows this application (`x-secondly-ratelimit-limit`).
    pub per_second_limit: Option<u64>,
    pub hourly_left: Option<u64>,
    pub hourly_limit: Option<u64>,
    /// Requests the API refused with 429 (sent again after a pause).
    pub throttled: u64,
    /// Responses taken from the stopped run being resumed (not sent again).
    pub reused: u64,
    /// Assertions of request blocks that held / didn't.
    pub asserts_passed: u64,
    pub asserts_failed: u64,
}

/// A running "for each": items done and the time left, estimated from the items done so far
/// (before the first one, from the requests of an item and the request rate).
#[derive(Clone)]
pub struct Progress {
    /// `p in projects.body`
    pub label: String,
    pub done: usize,
    pub total: usize,
    pub parallel: bool,
    pub left_secs: Option<f64>,
    /// Requests the rest of the loop should still send (to compare with the hourly limit).
    pub requests_left: Option<u64>,
}

impl Progress {
    /// `34/500 · ~1 min left`
    pub fn text(&self) -> String {
        let left = self.left_secs.map(|s| format!(" · ~{} left", human_duration(s))).unwrap_or_default();
        format!("{}/{}{left}", self.done, self.total)
    }
}

pub struct RunHandle {
    pub rx: Receiver<RunEvent>,
    pub stop: Arc<AtomicBool>,
    pub started: Instant,
}

#[derive(Clone)]
pub struct RunSettings {
    pub delay: Duration,
    /// Parallel loops: items in flight at once.
    pub parallel: usize,
    /// Parallel loops: requests per second; `0` = the limit the API announces (`x-secondly-ratelimit-limit`).
    pub parallel_rate: f64,
    pub max_pages: u32,
    /// Turns a request block's documented `values` into query parameters / body fields.
    pub doc: Arc<ApiDoc>,
    /// The workspace's flows, for `flow` blocks.
    pub flows: Arc<Vec<Flow>>,
    /// Substitutions (a login typed where an id is expected) and the lookups already done.
    pub subs: Arc<Vec<crate::subst::Rule>>,
    pub subs_cache: Arc<crate::subst::Cache>,
    /// Where the responses go as they arrive (to resume a stopped run), and those of the run resumed.
    pub checkpoint: Option<Arc<crate::resume::Checkpoint>>,
    /// "Run to here": stop once this block (uid) is done, the variables as they are then.
    pub stop_after: Option<u64>,
    /// The API profile its requests use (base URL, auth); empty = the active one.
    pub profile: String,
}

pub fn start(flow: Flow, agent: ureq::Agent, auth: SharedAuth, creds: Credentials, settings: RunSettings, notify: impl Fn() + Send + Sync + 'static) -> RunHandle {
    let (tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    std::thread::spawn(move || {
        let shared = Arc::new(Shared {
            next_slot: Mutex::new(Instant::now()),
            rate: Mutex::default(),
            per_second_limit: AtomicU64::new(0),
            warnings: AtomicUsize::new(0),
            timings: Mutex::default(),
        });
        let checkpoint = settings.checkpoint.clone();
        let mut runner = Runner {
            agent,
            auth,
            creds,
            settings,
            tx: tx.clone(),
            stops: vec![stop_flag],
            notify: &notify,
            stack: vec![flow.uid],
            shared,
            buffer: None,
            loops: 0,
            site: Vec::new(),
            touched: None,
        };
        // The active environment's values to start with; the flow's inputs replace them.
        let mut vars: Map<String, Value> = crate::env::current().into_iter().map(|(k, v)| (k, loose_value(&v))).collect();
        let mut result: Result<(), FlowError> = Ok(());
        for input in flow.inputs.iter().filter(|i| !i.name.trim().is_empty()) {
            match input.to_value() {
                Ok(v) => {
                    vars.insert(input.name.trim().to_owned(), v);
                }
                Err(e) => {
                    result = Err(e.into());
                    break;
                }
            }
        }
        runner.log(LogKind::Info, 0, format!("▶ Running “{}”", flow.name), None);
        if let Some(n) = checkpoint.as_ref().map(|c| c.len()).filter(|&n| n > 0) {
            runner.log(LogKind::Info, 0, format!("↺ Resuming: {n} responses of the stopped run are reused (not sent again), then it goes on"), None);
        }
        let result = result.and_then(|()| runner.exec(&flow.blocks, &mut vars, 0));
        if result.is_ok() {
            for (name, v) in flow.outputs_of(&vars) {
                runner.log(LogKind::Var, 0, format!("output {name} = {}", short(&v)), Some(preview(&v)));
            }
        }
        let (reused, asserts_ok, asserts_ko) = {
            let r = runner.shared.rate.lock();
            (r.reused, r.asserts_passed, r.asserts_failed)
        };
        let tests = if asserts_ok + asserts_ko > 0 { format!(" · {asserts_ok}/{} assertions passed", asserts_ok + asserts_ko) } else { String::new() };
        if reused > 0 {
            runner.log(LogKind::Info, 0, format!("↺ {reused} responses reused from the stopped run"), None);
        }
        // Before the last line: the console ends on how the run ended.
        if let Some(text) = timings_text(&runner.shared.timings.lock()) {
            runner.log(LogKind::Info, 0, "◷ Time per block (click for the list)".into(), Some(text));
        }
        let ok = match result {
            Ok(()) => {
                if let Some(c) = &checkpoint {
                    c.finish();
                }
                let text = match runner.shared.warnings.load(Ordering::Relaxed) {
                    0 => format!("✓ Finished{tests}"),
                    1 => format!("✓ Finished with 1 warning (in orange above){tests}"),
                    n => format!("✓ Finished with {n} warnings (in orange above){tests}"),
                };
                runner.log(if runner.shared.warnings.load(Ordering::Relaxed) == 0 { LogKind::Success } else { LogKind::Warning }, 0, text, None);
                true
            }
            Err(e) => {
                if e == FlowError::Paused {
                    runner.log(LogKind::Success, 0, "‖ Stopped after the chosen block: the variables at this point are in the Variables tab".into(), None);
                } else {
                    runner.log(LogKind::Error, 0, format!("✖ {e}"), None);
                }
                if let Some(c) = &checkpoint {
                    let kept = c.len() + c.added();
                    if kept == 0 {
                        c.finish();
                    } else {
                        runner.log(LogKind::Warning, 0, format!("‖ {kept} responses kept: Resume goes on from here (the same inputs, nothing sent twice)"), None);
                    }
                }
                e == FlowError::Paused
            }
        };
        let _ = tx.send(RunEvent::Finished { ok, vars: Value::Object(vars) });
        notify();
    });
    RunHandle { rx, stop, started: Instant::now() }
}

/// What the threads of a run (parallel loops) share: the request pacing and the live numbers.
pub(super) struct Shared {
    /// When the next request may start.
    next_slot: Mutex<Instant>,
    rate: Mutex<Rate>,
    /// `x-secondly-ratelimit-limit`, `0` until a response told it.
    per_second_limit: AtomicU64,
    warnings: AtomicUsize,
    /// Time spent per block (requests, sub-flows, waits): total, times run, what it is.
    timings: Mutex<HashMap<u64, (Duration, u32, String)>>,
}

/// The error of an item of a parallel loop stopped because another item failed.
/// Why blocks stopped running.
#[derive(Debug, Clone, PartialEq)]
pub enum FlowError {
    /// The user pressed Stop.
    Stopped,
    /// Another item of the same parallel loop (or page of the same list) failed: this one stops
    /// quietly, the other's error is the one shown.
    Aborted,
    /// "Run to here": the chosen block is done (not an error).
    Paused,
    /// Anything else, as the console shows it.
    Failed(String),
}

impl FlowError {
    /// Adds where it happened to a real error (`in flow “X”: …`); a stop stays a stop.
    fn context(self, f: impl FnOnce(&str) -> String) -> Self {
        match self {
            FlowError::Failed(e) => FlowError::Failed(f(&e)),
            other => other,
        }
    }

    /// A real error, to report (not a stop).
    fn is_failure(&self) -> bool {
        matches!(self, FlowError::Failed(_))
    }
}

impl std::fmt::Display for FlowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlowError::Stopped => f.write_str("Stopped by user"),
            FlowError::Aborted => f.write_str("Stopped (another item failed)"),
            FlowError::Paused => f.write_str("Stopped at the chosen block"),
            FlowError::Failed(e) => f.write_str(e),
        }
    }
}

impl From<String> for FlowError {
    fn from(e: String) -> Self {
        FlowError::Failed(e)
    }
}

impl From<&str> for FlowError {
    fn from(e: &str) -> Self {
        FlowError::Failed(e.to_owned())
    }
}

/// Adds a page of an "all pages" list; `false` when it is the last one (an error, a short page, the total reached).
pub(super) fn absorb_page(resp: ResponseData, size: u64, all: &mut Vec<Value>, last: &mut Option<ResponseData>, total_items: &mut Option<u64>) -> bool {
    let parsed: Value = serde_json::from_str(&resp.body).unwrap_or(Value::Null);
    let ok = resp.status < 400;
    let n = match parsed {
        Value::Array(a) => {
            let n = a.len();
            all.extend(a);
            n
        }
        other => {
            if all.is_empty() && !ok {
                *last = Some(resp);
                all.push(other);
                return false;
            }
            0
        }
    };
    let total = resp.header_u64("x-total");
    *total_items = total.or(*total_items);
    *last = Some(resp);
    ok && (n as u64) >= size && total.is_none_or(|t| (all.len() as u64) < t)
}

/// Variables an item of a parallel loop set, with their values.
pub(super) type Changes = Vec<(String, Value)>;

/// Items of a parallel loop done but not shown yet (the console shows them in order): result, log lines.
pub(super) type Pending = BTreeMap<usize, (Result<Option<Changes>, FlowError>, Vec<LogLine>)>;

pub(super) struct Runner<'a> {
    agent: ureq::Agent,
    auth: SharedAuth,
    creds: Credentials,
    settings: RunSettings,
    tx: Sender<RunEvent>,
    /// The user's Stop, then one per parallel loop this runner works for (set when another item failed).
    stops: Vec<Arc<AtomicBool>>,
    notify: &'a (dyn Fn() + Send + Sync),
    /// Flows being run (uids), to refuse a flow that ends up calling itself.
    stack: Vec<u64>,
    shared: Arc<Shared>,
    /// A worker of a parallel loop: its requests go out at the API's rate, and its log lines wait here
    /// until the items before are shown (the console reads in order).
    buffer: Option<RefCell<Vec<LogLine>>>,
    /// "For each" loops being run: only the outermost one reports its progress.
    loops: usize,
    /// Position of the running block: block indexes, `i<n>` for a loop item, `t0` / `e0` for an if's branch.
    site: Vec<String>,
    /// A worker of a parallel loop: the variables its item set, to put the ones from before the loop back
    /// for the next item (cheaper than copying every variable, big responses included, for each item).
    touched: Option<Vec<String>>,
}

/// Requests an item of a loop over `blocks` sends, roughly (`if`: the bigger branch, a nested loop: one turn).
pub(super) fn requests_in(blocks: &[Block], flows: &[Flow], depth: usize) -> f64 {
    if depth > 16 {
        return 0.0;
    }
    blocks
        .iter()
        .filter(|b| b.enabled)
        .map(|b| match &b.kind {
            BlockKind::Request(_) => 1.0,
            BlockKind::ForEach { blocks, .. } => requests_in(blocks, flows, depth + 1),
            BlockKind::If { then_blocks, else_blocks, .. } => requests_in(then_blocks, flows, depth + 1).max(requests_in(else_blocks, flows, depth + 1)),
            BlockKind::Flow(c) => find_flow(flows.iter(), c.flow_uid, &c.flow).map_or(0.0, |f| requests_in(&f.blocks, flows, depth + 1)),
            _ => 0.0,
        })
        .sum()
}

/// Progress of the outermost "for each", sent to the console.
pub(super) struct LoopTracker {
    label: String,
    total: usize,
    parallel: bool,
    started: Instant,
    requests_at_start: u64,
    /// Requests per item counted in the blocks, for the estimate before the first item is done.
    per_item_guess: f64,
    last_sent: Option<Instant>,
    /// Items done before the first request was sent (taken from a stopped run).
    replayed: usize,
}

impl LoopTracker {
    fn report(&mut self, r: &Runner, done: usize, force: bool) {
        let now = Instant::now();
        if !force && done < self.total && self.last_sent.is_some_and(|t| now - t < Duration::from_millis(150)) {
            return;
        }
        self.last_sent = Some(now);
        let left = self.total.saturating_sub(done);
        let sent = r.shared.rate.lock().requests - self.requests_at_start;
        // A resumed run replays its first items in no time: the estimate starts after them.
        if sent == 0 && done > 0 {
            self.started = now;
            self.replayed = done;
        }
        let live = done - self.replayed.min(done);
        let (left_secs, requests_left) = if live > 0 {
            let per_item = sent as f64 / live as f64;
            (Some(self.started.elapsed().as_secs_f64() / live as f64 * left as f64), (sent > 0).then(|| (per_item * left as f64).round() as u64))
        } else if self.per_item_guess > 0.0 {
            // Parallel: at the API's rate; one after the other: the delay, or a usual response time when longer.
            let per_request = match (self.parallel, r.parallel_rate()) {
                (true, Some(rate)) => 1.03 / rate,
                _ => r.settings.delay.as_secs_f64().max(0.35),
            };
            let n = self.per_item_guess * left as f64;
            (Some(n * per_request), Some(n.round() as u64))
        } else {
            (None, None)
        };
        let p = Progress { label: self.label.clone(), done, total: self.total, parallel: self.parallel, left_secs, requests_left };
        let _ = r.tx.send(RunEvent::Progress(Some(p)));
        (r.notify)();
    }
}

/// `45 s`, `3 min`, `1 h 05`.
/// The blocks whose time is counted (what takes time: calls and waits), and how the summary names them.
fn timed(b: &Block) -> Option<String> {
    match &b.kind {
        BlockKind::Request(r) => Some(format!("{} {}", r.verb, r.path)),
        BlockKind::Flow(c) => Some(format!("flow “{}”", c.flow)),
        BlockKind::Wait { .. } => Some("wait".into()),
        _ => None,
    }
}

/// The time per block, slowest first (one line each), or None when nothing took time.
fn timings_text(timings: &HashMap<u64, (Duration, u32, String)>) -> Option<String> {
    let mut rows: Vec<&(Duration, u32, String)> = timings.values().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    (!rows.is_empty()).then(|| {
        rows.iter()
            .map(|(d, n, what)| {
                let times = if *n > 1 { format!(" ×{n}") } else { String::new() };
                format!("{:>9}{times:<7} {what}", human_duration(d.as_secs_f64()))
            })
            .collect::<Vec<_>>()
            .join("\n")
    })
}

pub(super) fn human_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    match s {
        0..=59 => format!("{s} s"),
        60..=3599 => format!("{} min", (s + 30) / 60),
        _ => format!("{} h {:02}", s / 3600, (s % 3600) / 60),
    }
}

/// The first 4000 bytes of `v` as pretty JSON. Serialization stops there: a 6 MB response isn't written out whole
/// just to keep its beginning.
pub(super) fn preview(v: &Value) -> String {
    const MAX: usize = 4000;
    struct Capped(Vec<u8>);
    impl std::io::Write for Capped {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let room = MAX + 4 - self.0.len();
            self.0.extend_from_slice(&buf[..buf.len().min(room)]);
            if buf.len() > room {
                return Err(std::io::Error::other("full"));
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Capped(Vec::with_capacity(MAX + 4));
    let cut = serde_json::to_writer_pretty(&mut out, v).is_err();
    let mut s = String::from_utf8_lossy(&out.0).into_owned();
    if cut || s.len() > MAX {
        let mut end = MAX.min(s.len());
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n…");
    }
    s
}

pub(super) fn short(v: &Value) -> String {
    let s = match v {
        Value::String(s) => format!("\"{s}\""),
        Value::Array(a) => format!("list of {} items", a.len()),
        Value::Object(o) => format!("object with {} fields", o.len()),
        other => other.to_string(),
    };
    if s.chars().count() > 120 {
        format!("{}…", s.chars().take(120).collect::<String>())
    } else {
        s
    }
}

impl<'a> Runner<'a> {
    fn log(&self, kind: LogKind, depth: usize, text: String, detail: Option<String>) {
        self.emit(LogLine { kind, depth, text, detail });
    }

    fn emit(&self, line: LogLine) {
        match &self.buffer {
            Some(b) => b.borrow_mut().push(line),
            None => {
                let _ = self.tx.send(RunEvent::Log(line));
                (self.notify)();
            }
        }
    }

    fn warn(&self, depth: usize, text: String) {
        self.shared.warnings.fetch_add(1, Ordering::Relaxed);
        self.log(LogKind::Warning, depth, text, None);
    }

    fn check_stop(&self) -> Result<(), FlowError> {
        if self.stops[0].load(Ordering::Relaxed) {
            Err(FlowError::Stopped)
        } else if self.stops[1..].iter().any(|s| s.load(Ordering::Relaxed)) {
            Err(FlowError::Aborted)
        } else {
            Ok(())
        }
    }

    /// A runner for one thread of a parallel loop (`abort` is set when an item fails).
    fn worker(&self, abort: Arc<AtomicBool>) -> Runner<'a> {
        let mut stops = self.stops.clone();
        stops.push(abort);
        Runner {
            agent: self.agent.clone(),
            auth: self.auth.clone(),
            creds: self.creds.clone(),
            settings: self.settings.clone(),
            tx: self.tx.clone(),
            stops,
            notify: self.notify,
            stack: self.stack.clone(),
            shared: self.shared.clone(),
            buffer: Some(RefCell::default()),
            loops: self.loops,
            site: self.site.clone(),
            touched: None,
        }
    }

    /// Sets a variable (blocks never change variables any other way).
    fn set_var(&mut self, vars: &mut Map<String, Value>, name: String, value: Value) {
        if let Some(t) = &mut self.touched {
            t.push(name.clone());
        }
        vars.insert(name, value);
    }

    /// Requests per second of parallel loops: the one set in Settings, else the API's limit once known.
    fn parallel_rate(&self) -> Option<f64> {
        if self.settings.parallel_rate > 0.0 {
            return Some(self.settings.parallel_rate);
        }
        match self.shared.per_second_limit.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n as f64),
        }
    }

    /// Time between the starts of two requests. In a parallel loop, a little more than the API's
    /// per-second limit allows (a 429 costs a pause of everyone); elsewhere, the delay of the Settings.
    fn gap(&self) -> Duration {
        match (self.buffer.is_some(), self.parallel_rate()) {
            (true, Some(rate)) => Duration::from_secs_f64(1.03 / rate.max(0.01)),
            _ => self.settings.delay,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn for_each(&mut self, b: &Block, list: &str, item: &str, blocks: &[Block], parallel: bool, vars: &mut Map<String, Value>, depth: usize) -> Result<(), FlowError> {
        let verbose = b.logs();
        let inner = depth + verbose as usize;
        let v = lookup(vars, list).ok_or_else(|| format!("For each: {} not found", bare(list)))?;
        let items: Vec<Value> = match v {
            Value::Array(a) => a,
            Value::Object(o) => o.into_iter().map(|(k, v)| serde_json::json!({ "key": k, "value": v })).collect(),
            Value::Null => Vec::new(),
            other => vec![other],
        };
        let parallel = parallel && items.len() > 1 && self.settings.parallel > 1;
        let item = item.trim();
        if verbose {
            let how = if parallel { format!(", {} at a time", self.settings.parallel.min(items.len())) } else { String::new() };
            self.log(LogKind::Info, depth, format!("↻ For each {item} in {} ({} items{how})", bare(list), items.len()), None);
        }
        let mut tracker = (self.loops == 0 && self.buffer.is_none()).then(|| LoopTracker {
            label: format!("{item} in {}", bare(list)),
            total: items.len(),
            parallel,
            started: Instant::now(),
            requests_at_start: self.shared.rate.lock().requests,
            per_item_guess: requests_in(blocks, &self.settings.flows, 0),
            last_sent: None,
            replayed: 0,
        });
        let started = Instant::now();
        let requests_before = self.shared.rate.lock().requests;
        let total = items.len();
        // A loop inside a loop: the outer {{index}} comes back after it.
        let outer_index = vars.get("index").cloned();
        self.loops += 1;
        let result = if parallel {
            self.for_each_parallel(blocks, items, item, vars, inner, &mut tracker)
        } else {
            let mut result = Ok(());
            for (i, it) in items.into_iter().enumerate() {
                if let Some(t) = &mut tracker {
                    t.report(self, i, false);
                }
                result = self.check_stop().and_then(|()| {
                    self.set_var(vars, item.to_owned(), it);
                    self.set_var(vars, "index".into(), Value::from(i));
                    self.site.push(format!("i{i}"));
                    let r = self.exec(blocks, vars, inner);
                    self.site.pop();
                    r
                });
                if result.is_err() {
                    break;
                }
            }
            result
        };
        self.loops -= 1;
        if let Some(v) = outer_index {
            self.set_var(vars, "index".into(), v);
        }
        if tracker.is_some() {
            let _ = self.tx.send(RunEvent::Progress(None));
        }
        result?;
        let sent = self.shared.rate.lock().requests - requests_before;
        if verbose && sent > 0 && total > 1 {
            let secs = started.elapsed().as_secs_f64();
            self.log(LogKind::Info, depth, format!("↻ {total} items in {} · {sent} requests ({:.1}/s)", human_duration(secs), sent as f64 / secs.max(0.001)), None);
        }
        Ok(())
    }

    /// Items handed out to `settings.parallel` threads; their log lines are shown in item order.
    fn for_each_parallel(
        &mut self,
        blocks: &[Block],
        items: Vec<Value>,
        item: &str,
        vars: &mut Map<String, Value>,
        depth: usize,
        tracker: &mut Option<LoopTracker>,
    ) -> Result<(), FlowError> {
        let total = items.len();
        let abort = Arc::new(AtomicBool::new(false));
        // "Run to here" reached in one item: the others stop, its variables are the ones kept.
        let paused = AtomicBool::new(false);
        let next = AtomicUsize::new(0);
        // Per item: its result, with the variables it set when it's the last item (they're kept after the loop).
        let (done_tx, done_rx) = mpsc::channel::<(usize, Result<Option<Changes>, FlowError>, Vec<LogLine>)>();
        let base: &Map<String, Value> = vars;
        let mut pending = BTreeMap::new();
        let mut shown = 0usize;
        let mut done = 0;
        let mut first_error: Option<FlowError> = None;
        let mut last_vars = None;
        if let Some(t) = tracker.as_mut() {
            t.report(self, 0, true);
        }
        std::thread::scope(|s| {
            for _ in 0..self.settings.parallel.min(total) {
                let mut w = self.worker(abort.clone());
                let (done_tx, next, abort, items, paused) = (done_tx.clone(), &next, &abort, &items, &paused);
                s.spawn(move || {
                    // One copy of the variables per thread (not per item): each item starts from it, and what
                    // it set is put back the way it was before the loop for the next one.
                    let mut copy: Option<Map<String, Value>> = None;
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        if i >= total || w.check_stop().is_err() {
                            break;
                        }
                        let v = copy.get_or_insert_with(|| base.clone());
                        w.touched = Some(Vec::new());
                        w.set_var(v, item.to_owned(), items[i].clone());
                        w.set_var(v, "index".into(), Value::from(i));
                        w.site.push(format!("i{i}"));
                        let mut result = w.exec(blocks, v, depth);
                        w.site.pop();
                        let pause = result == Err(FlowError::Paused);
                        if pause {
                            paused.store(true, Ordering::Relaxed);
                            result = Ok(());
                        }
                        if result.is_err() || pause {
                            abort.store(true, Ordering::Relaxed);
                        }
                        let mut touched = w.touched.take().unwrap_or_default();
                        touched.sort();
                        touched.dedup();
                        let last = (i == total - 1 || pause).then(|| touched.iter().filter_map(|k| Some((k.clone(), v.get(k)?.clone()))).collect::<Changes>());
                        for k in touched {
                            match base.get(&k) {
                                Some(before) => {
                                    v.insert(k, before.clone());
                                }
                                None => {
                                    v.shift_remove(&k);
                                }
                            }
                        }
                        let lines = w.buffer.as_ref().map(|b| b.take()).unwrap_or_default();
                        let _ = done_tx.send((i, result.map(|()| last), lines));
                    }
                });
            }
            drop(done_tx);
            // Shows the items done in order (`all`: also those after a missing one).
            let mut release = |pending: &mut Pending, all: bool| {
                while let Some(entry) = pending.first_entry() {
                    if !all && *entry.key() != shown {
                        break;
                    }
                    let (result, lines) = entry.remove();
                    for line in lines {
                        self.emit(line);
                    }
                    match result {
                        Ok(v) => last_vars = v.or(last_vars.take()),
                        Err(e) if e.is_failure() && first_error.is_none() => first_error = Some(e),
                        Err(_) => {}
                    }
                    shown += 1;
                }
            };
            for (i, result, lines) in done_rx {
                pending.insert(i, (result, lines));
                done += 1;
                release(&mut pending, false);
                if let Some(t) = tracker.as_mut() {
                    t.report(self, done, false);
                }
            }
            // Items after one that failed or was stopped.
            release(&mut pending, true);
        });
        if let Some(e) = first_error {
            return Err(e);
        }
        self.check_stop()?;
        // The other items were stopped on purpose: the one that reached the block gives its variables.
        if paused.load(Ordering::Relaxed) {
            if let Some(changes) = last_vars {
                for (k, v) in changes {
                    self.set_var(vars, k, v);
                }
            }
            return Err(FlowError::Paused);
        }
        if let Some(changes) = last_vars {
            for (k, v) in changes {
                self.set_var(vars, k, v);
            }
        }
        Ok(())
    }

    fn exec(&mut self, blocks: &[Block], vars: &mut Map<String, Value>, depth: usize) -> Result<(), FlowError> {
        for (at, b) in blocks.iter().enumerate().filter(|(_, b)| b.enabled) {
            self.check_stop()?;
            // Where the block is (`3/i17/0`): a resumed run finds its responses there.
            self.site.push(at.to_string());
            let started = Instant::now();
            let result = self.exec_block(b, vars, depth);
            self.site.pop();
            if let Some(what) = timed(b) {
                let mut timings = self.shared.timings.lock();
                let t = timings.entry(b.uid).or_insert((Duration::ZERO, 0, what));
                t.0 += started.elapsed();
                t.1 += 1;
            }
            result?;
            if self.settings.stop_after == Some(b.uid) {
                return Err(FlowError::Paused);
            }
        }
        Ok(())
    }

    fn exec_block(&mut self, b: &Block, vars: &mut Map<String, Value>, depth: usize) -> Result<(), FlowError> {
        {
            let verbose = b.logs();
            // Children of a quiet block aren't indented under a line that isn't shown.
            let inner = depth + verbose as usize;
            match &b.kind {
                BlockKind::Request(r) => self.request(r, vars, depth, verbose)?,
                BlockKind::Extract { from, into } => {
                    let v = resolve(vars, from).ok_or_else(|| format!("Extract: {} not found", bare(from)))?;
                    if verbose {
                        self.log(LogKind::Var, depth, format!("{} = {}", into.trim(), short(&v)), Some(preview(&v)));
                    }
                    self.set_var(vars, into.trim().to_owned(), v);
                }
                BlockKind::Set { name, value } => {
                    let v = if single_placeholder(value) {
                        resolve(vars, value).ok_or_else(|| format!("Set: {} not found", bare(value)))?
                    } else {
                        loose_value(&render(value, vars)?)
                    };
                    if verbose {
                        self.log(LogKind::Var, depth, format!("{} = {}", name.trim(), short(&v)), None);
                    }
                    self.set_var(vars, name.trim().to_owned(), v);
                }
                BlockKind::ForEach { list, item, blocks, parallel } => self.for_each(b, list, item, blocks, *parallel, vars, depth)?,
                BlockKind::If { left, op, right, then_blocks, else_blocks } => {
                    let a = render_lenient(left, vars);
                    let b = render_lenient(right, vars);
                    let holds = compare(*op, &a, &b);
                    let shown = if op.unary() { format!("“{a}” {}", op.label()) } else { format!("“{a}” {} “{b}”", op.label()) };
                    if verbose {
                        self.log(LogKind::Info, depth, format!("? {shown} → {}", if holds { "yes" } else { "no" }), None);
                    }
                    self.site.push(if holds { "t0" } else { "e0" }.into());
                    let result = self.exec(if holds { then_blocks } else { else_blocks }, vars, inner);
                    self.site.pop();
                    result?;
                }
                // Resuming: a pause the stopped run already went past isn't made again.
                BlockKind::Wait { .. } if self.settings.checkpoint.as_ref().is_some_and(|c| c.passed(&self.site)) => {}
                BlockKind::Wait { ms } => {
                    let until = Instant::now() + Duration::from_millis(*ms);
                    while Instant::now() < until {
                        self.check_stop()?;
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
                BlockKind::Log { message } => {
                    if verbose {
                        self.log(LogKind::Info, depth, render_lenient(message, vars), None);
                    }
                }
                BlockKind::Fail { message } => return Err(render_lenient(message, vars).into()),
                BlockKind::Flow(c) => self.call_flow(c, vars, depth, verbose)?,
                BlockKind::WebSocket(w) => self.web_socket(w, vars, depth, verbose)?,
                BlockKind::Grpc(g) => self.grpc(g, vars, depth, verbose)?,
            }
        }
        Ok(())
    }

    /// The profile a block uses: its own, else the flow's.
    fn block_profile(&self, own: &str) -> String {
        if own.trim().is_empty() {
            self.settings.profile.clone()
        } else {
            own.trim().to_owned()
        }
    }

    /// `headers` (templates filled) and what `auth` adds — its own, else its profile's, else the
    /// workspace's; an OAuth token is fetched here.
    fn live_headers(&self, headers: &[Kv], auth: &crate::auth::Auth, profile: &str, vars: &Map<String, Value>) -> Result<Vec<(String, String)>, FlowError> {
        let mut out = Vec::new();
        for kv in headers.iter().filter(|kv| kv.enabled && !kv.key.trim().is_empty()) {
            out.push((render(kv.key.trim(), vars)?, render(&kv.value, vars)?));
        }
        let fill = |v: &str| crate::dates::expand(&crate::env::expand(&render_lenient(v, vars)));
        let profile_auth = crate::profiles::current(profile).map(|p| p.auth).unwrap_or_default();
        let auth = auth.or(profile_auth.or(&self.creds.auth)).expand(&fill);
        let (extra, _) = crate::auth::apply(&self.agent, &self.auth, &auth, &out, &crate::auth::Refresh::IfExpired)?;
        out.extend(extra);
        Ok(out)
    }

    /// A WebSocket conversation: connect, send, collect the answers (for `wait_ms` after the last
    /// activity, up to `max_messages`, or until a message has `until`), close.
    fn web_socket(&mut self, w: &WsBlock, vars: &mut Map<String, Value>, depth: usize, verbose: bool) -> Result<(), FlowError> {
        use crate::ws::{WsData, WsEvent};
        let profile = self.block_profile(&w.profile);
        let url = render(w.url.trim(), vars)?;
        let url = model::resolve_url_for(&crate::env::expand(&url), &profile);
        let url = match url.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some(("http", rest)) => format!("ws://{rest}"),
            _ => url,
        };
        let headers = self.live_headers(&w.headers, &w.auth, &profile, vars)?;
        if verbose {
            self.log(LogKind::Request, depth, format!("→ WebSocket {url}"), None);
        }
        let (tx, rx) = mpsc::channel();
        let cfg = crate::ws::WsConfig { url: url.clone(), headers, subprotocols: w.subprotocols.clone(), connect_timeout: Duration::from_secs(10) };
        let handle = crate::ws::connect(cfg, tx, Box::new(|| {}));
        let mut result = serde_json::json!({ "connected": false, "status": Value::Null, "protocol": Value::Null, "sent": 0, "received": [], "closed": Value::Null });
        let mut received: Vec<Value> = Vec::new();
        let mut error: Option<String> = None;
        // Connected (or not) within the connect timeout.
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            self.check_stop()?;
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(WsEvent::Connected { status, protocol, .. }) => {
                    result["connected"] = Value::Bool(true);
                    result["status"] = Value::from(status);
                    result["protocol"] = protocol.map(Value::from).unwrap_or(Value::Null);
                    break;
                }
                Ok(WsEvent::Error(e)) => {
                    error = Some(e);
                    break;
                }
                Ok(_) => {}
                Err(_) if Instant::now() > deadline => {
                    error = Some("no answer to the connection".into());
                    break;
                }
                Err(_) => {}
            }
        }
        if error.is_none() {
            for m in &w.messages {
                let text = crate::env::expand(&render(m, vars)?);
                if verbose {
                    self.log(LogKind::Info, depth + 1, format!("↑ {}", short(&Value::String(text.clone()))), Some(text.clone()));
                }
                handle.send_text(text);
            }
            result["sent"] = Value::from(w.messages.len());
            let wait = Duration::from_millis(w.wait_ms.max(1));
            let mut last = Instant::now();
            let until = render_lenient(&w.until, vars);
            'collect: loop {
                self.check_stop()?;
                if last.elapsed() >= wait {
                    break;
                }
                match rx.recv_timeout(Duration::from_millis(50)) {
                    Ok(WsEvent::Received { data, .. }) => {
                        last = Instant::now();
                        let (value, text) = match data {
                            WsData::Text(t) => (serde_json::from_str::<Value>(&t).unwrap_or_else(|_| Value::String(t.clone())), t),
                            WsData::Binary(b) => (Value::from(format!("{} bytes", b.len())), String::new()),
                        };
                        if verbose {
                            self.log(LogKind::Response, depth + 1, format!("↓ {}", short(&value)), Some(preview(&value)));
                        }
                        received.push(value);
                        if (w.max_messages > 0 && received.len() >= w.max_messages) || (!until.is_empty() && text.contains(&until)) {
                            break 'collect;
                        }
                    }
                    Ok(WsEvent::Sent { .. }) => last = Instant::now(),
                    Ok(WsEvent::Closed { code, reason }) => {
                        result["closed"] = serde_json::json!({ "code": code, "reason": reason });
                        break;
                    }
                    Ok(WsEvent::Error(e)) => {
                        error = Some(e);
                        break;
                    }
                    Ok(WsEvent::Connected { .. }) | Err(_) => {}
                }
            }
            handle.close(1000, String::new());
        }
        result["received"] = Value::Array(received);
        if let Some(e) = &error {
            result["error"] = Value::String(e.clone());
        }
        let n = result["received"].as_array().map_or(0, Vec::len);
        if verbose || error.is_some() {
            let kind = if error.is_some() { LogKind::Error } else { LogKind::Response };
            self.log(
                kind,
                depth,
                format!("← WebSocket · {} sent · {n} received{} → {}", w.messages.len(), error.as_ref().map(|e| format!(" · {e}")).unwrap_or_default(), w.save_as.trim()),
                None,
            );
        }
        self.set_var(vars, w.save_as.trim().to_owned(), result);
        match error {
            Some(e) if w.stop_on_error => Err(format!("WebSocket {url}: {e}").into()),
            _ => Ok(()),
        }
    }

    /// A gRPC call; its result in `save_as`. A non-OK status stops the flow when `stop_on_error`.
    fn grpc(&mut self, g: &GrpcBlock, vars: &mut Map<String, Value>, depth: usize, verbose: bool) -> Result<(), FlowError> {
        let profile = self.block_profile(&g.profile);
        let url = render(g.url.trim(), vars)?;
        let url = model::resolve_url_for(&crate::env::expand(&url), &profile);
        let url = url.trim_end_matches('/').to_owned();
        let metadata = self.live_headers(&g.metadata, &g.auth, &profile, vars)?;
        let timeout = Duration::from_millis(g.timeout_ms.max(1));
        let protos = if g.proto_files.is_empty() {
            crate::grpc::Protos::from_reflection(&url, &metadata, timeout)?
        } else {
            let files: Vec<std::path::PathBuf> = g.proto_files.iter().map(|f| model::expand_home(f)).collect();
            let includes: Vec<std::path::PathBuf> = g.proto_includes.iter().map(|f| model::expand_home(f)).collect();
            crate::grpc::Protos::from_files(&files, &includes)?
        };
        let method = render(g.method.trim(), vars)?;
        let info = protos
            .method(&method)
            .ok_or_else(|| format!("gRPC: no method {method} (services: {})", protos.services().iter().map(|s| s.name.clone()).collect::<Vec<_>>().join(", ")))?;
        let body = crate::env::expand(&render(&g.message, vars)?);
        let messages = match serde_json::from_str::<Value>(body.trim()) {
            Ok(Value::Array(a)) if info.client_streaming => a.iter().map(|v| v.to_string()).collect(),
            _ if body.trim().is_empty() => Vec::new(),
            _ => vec![body],
        };
        if verbose {
            self.log(LogKind::Request, depth, format!("→ gRPC {} ({})", info.full_path, info.kind()), Some(url.clone()));
        }
        self.check_stop()?;
        let req = crate::grpc::CallRequest { url: url.clone(), method_path: info.full_path.clone(), messages, metadata, timeout };
        let r = crate::grpc::call(&protos, &req, &mut |_| {})?;
        let parse = |m: &String| serde_json::from_str::<Value>(m).unwrap_or_else(|_| Value::String(m.clone()));
        let messages: Vec<Value> = r.messages.iter().map(parse).collect();
        let pairs = |l: &[(String, String)]| Value::Object(l.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect());
        let ok = r.status_code == 0;
        let result = serde_json::json!({
            "status": r.status_code, "status_name": r.status_name, "ok": ok, "status_message": r.status_message,
            "message": messages.first().cloned().unwrap_or(Value::Null), "messages": messages,
            "headers": pairs(&r.headers), "trailers": pairs(&r.trailers), "time": r.elapsed.as_millis() as u64,
        });
        if verbose || !ok {
            let kind = if ok { LogKind::Response } else { LogKind::Error };
            self.log(
                kind,
                depth,
                format!("← {} {} · {} message(s) → {}", r.status_code, r.status_name, r.messages.len(), g.save_as.trim()),
                Some(preview(&result["message"])),
            );
        }
        self.set_var(vars, g.save_as.trim().to_owned(), result);
        if !ok && g.stop_on_error {
            return Err(format!("gRPC {} returned {} {}: {}", info.full_path, r.status_code, r.status_name, r.status_message).into());
        }
        Ok(())
    }

    /// A request block's script: `pm.variables` are the flow's variables (texts; small objects as
    /// JSON), what it sets comes back as variables; its tests count like assertions. Returns the
    /// request as it left it (pre) and the tests' outcomes (post).
    fn block_script(
        &mut self,
        script: &str,
        phase: crate::script::Phase,
        request: crate::script::ScriptRequest,
        response: Option<crate::script::ScriptResponse>,
        vars: &mut Map<String, Value>,
        depth: usize,
    ) -> Result<(crate::script::ScriptRequest, Vec<crate::assert::Outcome>), FlowError> {
        if script.trim().is_empty() {
            return Ok((request, Vec::new()));
        }
        let text = |v: &Value| match v {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            other => Some(other.to_string()).filter(|t| t.len() <= 20_000),
        };
        let before: Vec<(String, String)> = vars.iter().filter_map(|(k, v)| Some((k.clone(), text(v)?))).collect();
        let input = crate::script::ScriptInput { request, response, environment: crate::env::current(), variables: before.clone(), globals: crate::env::globals() };
        let out = crate::script::run(script, phase, &input, &crate::script::Limits::default());
        crate::env::apply_script(&input.environment, &out.environment, crate::env::runtime(), out.globals.clone());
        for (level, line) in &out.logs {
            let kind = if matches!(level, crate::script::LogLevel::Error) { LogKind::Error } else { LogKind::Info };
            self.log(kind, depth + 1, format!("console: {line}"), None);
        }
        for (k, v) in &out.variables {
            if before.iter().find(|(b, _)| b == k).map(|(_, bv)| bv) != Some(v) {
                let value = serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.clone()));
                self.set_var(vars, k.clone(), value);
            }
        }
        if let Some(e) = out.error {
            let which = if phase == crate::script::Phase::Pre { "pre-request" } else { "post-response" };
            return Err(format!("{which} script: {e}").into());
        }
        if out.skip_request {
            return Err("skipped by the pre-request script (pm.execution.skipRequest)".into());
        }
        let outcomes = out
            .tests
            .iter()
            .map(|t| crate::assert::Outcome { text: format!("pm.test “{}”", t.name), passed: t.passed, detail: t.error.clone().unwrap_or_default() })
            .collect();
        Ok((out.request, outcomes))
    }

    fn call_flow(&mut self, c: &FlowCall, vars: &mut Map<String, Value>, depth: usize, verbose: bool) -> Result<(), FlowError> {
        let flows = self.settings.flows.clone();
        let sub = find_flow(flows.iter(), c.flow_uid, &c.flow).ok_or_else(|| format!("Run flow: no flow named “{}” in the workspace", c.flow.trim()))?;
        if self.stack.contains(&sub.uid) || self.stack.len() > 16 {
            return Err(format!("Run flow: “{}” ends up running itself", sub.name).into());
        }
        let fail = |e: String| format!("Run flow “{}”: {e}", sub.name);
        let mut sub_vars = Map::new();
        for input in sub.inputs.iter().filter(|i| !i.name.trim().is_empty()) {
            let name = input.name.trim();
            let v = match c.inputs.get(name).filter(|v| !v.trim().is_empty()) {
                Some(t) if single_placeholder(t) => resolve(vars, t).ok_or_else(|| fail(format!("{} not found (input {name})", bare(t))))?,
                Some(t) => FlowInput { value: render(t, vars).map_err(fail)?, ..input.clone() }.to_value().map_err(fail)?,
                None => input.to_value().map_err(fail)?,
            };
            sub_vars.insert(name.to_owned(), v);
        }
        if verbose {
            self.log(LogKind::Info, depth, format!("⇢ Run flow “{}”", sub.name), Some(preview(&Value::Object(sub_vars.clone()))));
        }
        self.stack.push(sub.uid);
        // The sub-flow's variables are its own: not the loop item's to put back. It may run with another profile.
        let touched = self.touched.take();
        let profile = (!c.profile.trim().is_empty()).then(|| std::mem::replace(&mut self.settings.profile, c.profile.trim().to_owned()));
        let result = self.exec(&sub.blocks, &mut sub_vars, depth + verbose as usize);
        if let Some(p) = profile {
            self.settings.profile = p;
        }
        self.touched = touched;
        self.stack.pop();
        // The stop sentinel stays as is: the loops tell it from a real error.
        result.map_err(|e| e.context(|e| format!("in flow “{}”: {e}", sub.name)))?;
        let out = Value::Object(sub.outputs_of(&sub_vars));
        let save_as = if c.save_as.trim().is_empty() { "sub" } else { c.save_as.trim() };
        if verbose {
            self.log(LogKind::Var, depth, format!("{save_as} = {}", short(&out)), Some(preview(&out)));
        }
        self.set_var(vars, save_as.to_owned(), out);
        Ok(())
    }

    fn send(&mut self, req: &Prepared) -> Result<ResponseData, FlowError> {
        let key = self.settings.checkpoint.as_ref().map(|_| format!("{}|{}", self.site.join("/"), crate::resume::signature(req)));
        if let (Some(c), Some(k)) = (&self.settings.checkpoint, &key) {
            if let Some(resp) = c.get(k) {
                self.check_stop()?;
                let rate = {
                    let mut rate = self.shared.rate.lock();
                    rate.reused += 1;
                    *rate
                };
                let _ = self.tx.send(RunEvent::Rate(rate));
                return Ok(resp);
            }
        }
        let mut refused = 0;
        loop {
            // Every thread of the run takes the next free slot, so parallel items share the API's rate.
            let slot = {
                let mut next = self.shared.next_slot.lock();
                let slot = (*next).max(Instant::now());
                *next = slot + self.gap();
                slot
            };
            loop {
                self.check_stop()?;
                let now = Instant::now();
                if now >= slot {
                    break;
                }
                std::thread::sleep((slot - now).min(Duration::from_millis(50)));
            }
            let resp = net::execute(&self.agent, &self.auth, &self.creds, req)?;
            let header = |name: &str| resp.header_u64(name);
            if let Some(limit) = header("x-secondly-ratelimit-limit") {
                self.shared.per_second_limit.store(limit, Ordering::Relaxed);
            }
            let throttled = resp.status == 429 && refused < 5;
            let rate = {
                let mut rate = self.shared.rate.lock();
                rate.requests += 1;
                rate.throttled += throttled as u64;
                rate.per_second_left = header("x-secondly-ratelimit-remaining").or(rate.per_second_left);
                rate.per_second_limit = header("x-secondly-ratelimit-limit").or(rate.per_second_limit);
                rate.hourly_left = header("x-hourly-ratelimit-remaining").or(rate.hourly_left);
                rate.hourly_limit = header("x-hourly-ratelimit-limit").or(rate.hourly_limit);
                *rate
            };
            let _ = self.tx.send(RunEvent::Rate(rate));
            (self.notify)();
            if !throttled {
                if let (Some(c), Some(k)) = (&self.settings.checkpoint, &key) {
                    c.put(k, &resp);
                }
                return Ok(resp);
            }
            // Too many requests: everyone waits (Retry-After, else a second), then this one goes again.
            refused += 1;
            let pause = Duration::from_secs(header("retry-after").unwrap_or(1).clamp(1, 60));
            let mut next = self.shared.next_slot.lock();
            *next = (*next).max(Instant::now() + pause);
        }
    }

    fn request(&mut self, r: &RequestBlock, vars: &mut Map<String, Value>, depth: usize, verbose: bool) -> Result<(), FlowError> {
        let verb = r.verb.trim().to_uppercase();
        // Its auth, with the flow's variables in it (`{{token}}` from an earlier block).
        let auth = r.auth.expand(&|v: &str| super::template::render_lenient(v, vars));
        let mut notes = Vec::new();
        let rules = self.settings.subs.clone();
        let cache = self.settings.subs_cache.clone();
        let path = {
            let mut fix = |key: &str, value: String| -> Result<String, String> {
                let mut one = vec![(key.to_owned(), value)];
                crate::subst::apply_pairs(&mut one, &rules, &mut |rule, v| cache.resolve(rule, v, &mut |p| self.send(p).map_err(|e| e.to_string())), &mut notes)?;
                Ok(one.pop().map(|(_, v)| v).unwrap_or_default())
            };
            resolve_path_with(&r.path, vars, &mut fix)?
        };
        let mut query = Vec::new();
        for kv in r.query.iter().filter(|kv| kv.enabled && !kv.key.trim().is_empty()) {
            query.push((render(kv.key.trim(), vars)?, render(&kv.value, vars)?));
        }
        crate::subst::apply_pairs(&mut query, &rules, &mut |rule, v| cache.resolve(rule, v, &mut |p| self.send(p).map_err(|e| e.to_string())), &mut notes)?;
        let mut headers = Vec::new();
        for kv in r.headers.iter().filter(|kv| kv.enabled && !kv.key.trim().is_empty()) {
            headers.push((render(kv.key.trim(), vars)?, render(&kv.value, vars)?));
        }
        // The body: JSON (templates inside), or GraphQL, or text of its own type.
        let graphql = !r.graphql.trim().is_empty() && model::has_body(&verb);
        let as_text = !r.content_type.trim().is_empty() && model::has_body(&verb);
        let mut body = if model::has_body(&verb) && !graphql && !as_text { render_json(&r.body, vars)? } else { None };
        let mut files = Vec::new();
        if model::has_body(&verb) {
            for (k, v) in model::file_rows(&r.files) {
                files.push((render(k, vars)?, render(v, vars)?));
            }
        }
        // Documented fields filled in the block's form.
        // A file written by hand may leave `doc` out: the endpoint is then found from the verb and path.
        let docs = self.settings.doc.clone();
        let found = if r.values.is_empty() { None } else { r.doc.clone().or_else(|| docs.match_route(&verb, &r.path)) };
        if let Some((d, m)) = found.as_ref().and_then(|d| Some((d, docs.lookup(d)?.1))) {
            let mut values = BTreeMap::new();
            for (k, v) in r.values.iter().filter(|(_, v)| !v.trim().is_empty()) {
                values.insert(k.clone(), render(v, vars)?);
            }
            crate::subst::apply_map(&mut values, &rules, &mut |rule, v| cache.resolve(rule, v, &mut |p| self.send(p).map_err(|e| e.to_string())), &mut notes)?;
            let route = m.routes.get(d.route).map_or_else(|| r.path.clone(), |rt| rt.path.clone());
            let spec = RequestSpec { path: route, values, ..Default::default() };
            if model::has_body(&verb) {
                let mut form = model::form_body(m, &spec);
                if let Some(manual) = body.take() {
                    merge_json(&mut form, manual);
                }
                body = form.as_object().is_some_and(|o| !o.is_empty()).then_some(form);
                files.extend(model::form_files(m, &spec));
            } else {
                let manual: Vec<String> = query.iter().map(|(k, _)| k.clone()).collect();
                let form: Vec<(String, String)> = model::form_query(m, &spec).into_iter().filter(|(k, _)| !manual.contains(k)).collect();
                query.splice(0..0, form);
            }
        }
        let (body, content_type) = if graphql {
            let q = render(&r.graphql, vars)?;
            let v = render(&r.graphql_variables, vars)?;
            (Some(crate::graphql::request_body(&q, &v, "")?), None)
        } else if as_text {
            (Some(render(&r.body, vars)?).filter(|b| !b.is_empty()), Some(r.content_type.trim().to_owned()))
        } else {
            (body.map(|v| v.to_string()), None)
        };
        if verbose {
            for n in notes {
                self.log(LogKind::Info, depth, n, None);
            }
        }
        let profile = self.block_profile(&r.profile);
        let build = |query: &[(String, String)]| {
            let q = query.iter().map(|(k, v)| format!("{}={}", model::encode_component(k), model::encode_component(v))).collect::<Vec<_>>().join("&");
            let url = model::resolve_url_for(&if q.is_empty() { path.clone() } else { format!("{path}?{q}") }, &profile);
            Prepared {
                verb: verb.clone(),
                display_url: url.clone(),
                url,
                path: path.clone(),
                query: query.to_vec(),
                headers: headers.clone(),
                body: body.clone(),
                files: files.clone(),
                missing_path: Vec::new(),
                missing_required: Vec::new(),
                body_error: None,
                auth: auth.clone(),
                content_type: content_type.clone(),
                profile: profile.clone(),
            }
        };

        // An "all pages" list cut by the page limit: (fetched, total when known).
        let mut cut: Option<(usize, Option<u64>)> = None;
        let mut total_items: Option<u64> = None;
        let (status, resp_headers, body_value, items_note, millis) = if r.all_pages && verb == "GET" {
            let limit = match r.max_pages.unwrap_or(self.settings.max_pages) {
                0 => u32::MAX,
                n => n,
            };
            let size: u64 = query.iter().find(|(k, _)| k == "page[size]").and_then(|(_, v)| v.parse().ok()).unwrap_or(100).clamp(1, 100);
            query.retain(|(k, _)| k != "page[size]" && k != "page[number]");
            let mut all = Vec::new();
            let mut last: Option<ResponseData> = None;
            let started = Instant::now();
            let page_req = |page: u32| {
                let mut q = query.clone();
                q.push(("page[size]".into(), size.to_string()));
                q.push(("page[number]".into(), page.to_string()));
                build(&q)
            };
            // Once the first page told the total: "page 5/40 · ~1 min left".
            let progress = |page: u32, total_items: Option<u64>| match total_items {
                Some(t) => {
                    let pages = t.div_ceil(size.max(1)).max(1);
                    let until = pages.min(limit as u64);
                    let done = (page - 1) as u64;
                    let left = started.elapsed().as_secs_f64() / done.max(1) as f64 * until.saturating_sub(done) as f64;
                    let cap = if pages > limit as u64 { format!(", stops at {limit} (page limit)") } else { String::new() };
                    format!("page {page}/{pages} · ~{} left{cap}", human_duration(left))
                }
                None => format!("page {page}"),
            };
            let mut page = 1;
            while page <= limit {
                let req = page_req(page);
                if verbose {
                    self.log(LogKind::Request, depth, format!("→ {verb} {path} ({})", progress(page, total_items)), Some(req.url.clone()));
                }
                let resp = self.send(&req)?;
                if !absorb_page(resp, size, &mut all, &mut last, &mut total_items) {
                    break;
                }
                if page == limit {
                    cut = Some((all.len(), total_items));
                    break;
                }
                // The total is known: the other pages go out at once, at the API's per-second rate.
                let until = total_items.map_or(0, |t| t.div_ceil(size.max(1)).min(limit as u64) as u32);
                let threads = self.settings.parallel.min(until.saturating_sub(page) as usize);
                if threads < 2 {
                    page += 1;
                    continue;
                }
                let reqs: Vec<Prepared> = (page + 1..=until).map(page_req).collect();
                let abort = Arc::new(AtomicBool::new(false));
                let next = AtomicUsize::new(0);
                let (done_tx, done_rx) = mpsc::channel::<(usize, Result<ResponseData, FlowError>)>();
                let mut error: Option<FlowError> = None;
                let mut more = true;
                std::thread::scope(|s| {
                    for _ in 0..threads {
                        let mut w = self.worker(abort.clone());
                        let (done_tx, next, abort, reqs) = (done_tx.clone(), &next, &abort, &reqs);
                        s.spawn(move || loop {
                            let i = next.fetch_add(1, Ordering::SeqCst);
                            if i >= reqs.len() {
                                break;
                            }
                            let result = w.send(&reqs[i]);
                            if result.is_err() {
                                abort.store(true, Ordering::Relaxed);
                            }
                            let _ = done_tx.send((i, result));
                        });
                    }
                    drop(done_tx);
                    // Pages are added in order, whatever order they arrive in.
                    let mut pending = BTreeMap::new();
                    let mut shown = 0;
                    for (i, result) in done_rx {
                        pending.insert(i, result);
                        while let Some(result) = pending.remove(&shown) {
                            let at = shown;
                            shown += 1;
                            match result {
                                Ok(resp) if more && error.is_none() => {
                                    if verbose {
                                        self.log(
                                            LogKind::Request,
                                            depth,
                                            format!("→ {verb} {path} ({})", progress(page + 1 + at as u32, total_items)),
                                            Some(reqs[at].url.clone()),
                                        );
                                    }
                                    more = absorb_page(resp, size, &mut all, &mut last, &mut total_items);
                                }
                                Ok(_) => {}
                                Err(e) if e.is_failure() && error.is_none() => error = Some(e),
                                Err(_) => {}
                            }
                        }
                    }
                });
                if let Some(e) = error {
                    return Err(e);
                }
                if !more {
                    break;
                }
                if until == limit {
                    cut = Some((all.len(), total_items));
                    break;
                }
                // The list grew in the meantime: on with the next pages.
                page = until + 1;
            }
            if let Some((fetched, total)) = cut {
                let of = total.map(|t| format!(" of {t}")).unwrap_or_default();
                let fix = if r.max_pages.is_some() {
                    "raise “max pages” on this block (0 = no limit)"
                } else {
                    "set “max pages” on this block (0 = no limit) or raise it in Settings"
                };
                // Shown even for a quiet block: the list is incomplete.
                self.warn(depth, format!("⚠ page limit ({limit}) reached: {fetched}{of} items fetched, the rest is missing — {fix}"));
            }
            let last = last.ok_or("no page fetched")?;
            let note = format!("{} items", all.len());
            let body = if last.status < 400 { Value::Array(all) } else { serde_json::from_str(&last.body).unwrap_or(Value::Null) };
            (last.status, last.headers, body, note, started.elapsed().as_millis() as u64)
        } else {
            let mut req = build(&query);
            // The pre-request script can change it (and the variables).
            if !r.pre_script.trim().is_empty() {
                let (changed, _) = self.block_script(&r.pre_script, crate::script::Phase::Pre, crate::testrun::script_request(&req), None, vars, depth)?;
                crate::testrun::apply_request(&mut req, &changed);
            }
            if verbose {
                let uploads: String = req.files.iter().map(|(k, p)| format!("\nfile {k} ← {p}")).collect();
                self.log(LogKind::Request, depth, format!("→ {verb} {path}"), Some(format!("{}{uploads}", req.url)));
            }
            let resp = self.send(&req)?;
            let body: Value = serde_json::from_str(&resp.body).unwrap_or_else(|_| Value::String(resp.body.clone()));
            let note = match &body {
                Value::Array(a) => format!("{} items", a.len()),
                _ => format!("{} ms", resp.elapsed.as_millis()),
            };
            let millis = resp.elapsed.as_millis() as u64;
            (resp.status, resp.headers, body, note, millis)
        };

        let ok = status < 400;
        // HTTP errors are always shown, even for a quiet block.
        if verbose || !ok {
            self.log(if ok { LogKind::Response } else { LogKind::Error }, depth, format!("← {status} · {items_note} → {}", r.save_as.trim()), Some(preview(&body_value)));
        }
        let headers_obj: Map<String, Value> = resp_headers.into_iter().map(|(k, v)| (k, Value::String(v))).collect();
        let summary = error_text(&body_value);
        self.set_var(
            vars,
            r.save_as.trim().to_owned(),
            serde_json::json!({
                "status": status, "ok": ok, "headers": headers_obj, "body": body_value,
                // "All pages": how many exist, and whether the page limit cut the list.
                "total": total_items, "truncated": cut.is_some(),
            }),
        );
        // The post-response script's `pm.test`s count like assertions.
        let script_tests = if r.post_script.trim().is_empty() {
            Vec::new()
        } else {
            let res = vars.get(r.save_as.trim()).cloned().unwrap_or_default();
            let response = crate::script::ScriptResponse {
                status,
                status_text: String::new(),
                headers: res["headers"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned())).collect(),
                body: match &res["body"] {
                    Value::String(t) => t.clone(),
                    other => other.to_string(),
                },
                time_ms: millis,
            };
            let request = crate::script::ScriptRequest { method: verb.clone(), url: path.clone(), ..Default::default() };
            self.block_script(&r.post_script, crate::script::Phase::Post, request, Some(response), vars, depth)?.1
        };
        // With assertions, they decide (a 404 can be what is expected); otherwise the status does.
        if !r.assertions.is_empty() || !script_tests.is_empty() {
            let res = vars.get(r.save_as.trim()).cloned().unwrap_or_default();
            let headers: Map<String, Value> = res["headers"].as_object().into_iter().flatten().map(|(k, v)| (k.to_lowercase(), v.clone())).collect();
            let ctx = crate::assert::res_context(status, headers, res["body"].clone(), millis, 0);
            let mut outcomes = crate::assert::check(&r.assertions, &ctx, &|v| render_lenient(v, vars));
            outcomes.extend(script_tests);
            let failed = outcomes.iter().filter(|o| !o.passed).count();
            for o in &outcomes {
                if o.passed && verbose {
                    self.log(LogKind::Success, depth + 1, format!("✓ {}", o.text), None);
                } else if !o.passed {
                    self.log(LogKind::Error, depth + 1, format!("✗ {} — {}", o.text, o.detail), None);
                }
            }
            let rate = {
                let mut rate = self.shared.rate.lock();
                rate.asserts_passed += (outcomes.len() - failed) as u64;
                rate.asserts_failed += failed as u64;
                *rate
            };
            let _ = self.tx.send(RunEvent::Rate(rate));
            let _ = self.tx.send(RunEvent::Tests(outcomes));
            if failed > 0 {
                let what = format!("{failed} assertion{} failed on {verb} {path}", if failed > 1 { "s" } else { "" });
                if r.stop_on_error {
                    return Err(what.into());
                }
                self.warn(depth, format!("⚠ {what}"));
            }
            return Ok(());
        }
        if !ok && r.stop_on_error {
            return Err(format!("{verb} {path} returned {status}{summary}").into());
        }
        Ok(())
    }
}

pub(super) fn error_text(body: &Value) -> String {
    for key in ["message", "error", "errors"] {
        if let Some(v) = body.get(key) {
            return format!(": {}", to_text(v));
        }
    }
    String::new()
}
