//! Test runner, like Bruno's Runner: sends every saved request of a folder (or the whole workspace)
//! and runs its flows, one after the other, and reports what passed.
//!
//! A request passes when all its assertions hold; one without assertions passes on a status below 400.
//! A flow passes when it finishes (its request blocks can have assertions too).
//! Used by the app (folder menu › Run tests) and `apifa test` (exit status 1 on failure, `--junit`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::assert::{self, Outcome};
use crate::docs::ApiDoc;
use crate::flow::{self, Flow, LogKind, RunEvent, RunSettings};
use crate::model::{self, RequestSpec};
use crate::net::{self, Credentials, ResponseData, SharedAuth};
use crate::subst::{Cache, Rule};
use crate::workspace::{Folder, Node, SavedRequest};

/// What a saved request needs to go out: credentials, docs (documented fields), substitutions.
#[derive(Clone)]
pub struct Sender {
    pub agent: ureq::Agent,
    pub auth: SharedAuth,
    pub creds: Credentials,
    pub doc: Arc<ApiDoc>,
    pub rules: Arc<Vec<Rule>>,
    pub cache: Arc<Cache>,
}

impl Sender {
    /// Substitutions first (a login typed in `user_id` → its id), then the call; `notes` say what was replaced.
    pub fn send(&self, spec: &RequestSpec) -> Result<ResponseData, String> {
        let method = |spec: &RequestSpec| spec.doc.as_ref().and_then(|d| self.doc.lookup(d)).map(|(_, m)| m);
        let first = model::prepare(spec, method(spec));
        if !first.missing_path.is_empty() {
            return Err(format!("path variable(s) not filled: {}", first.missing_path.iter().map(|v| format!(":{v}")).collect::<Vec<_>>().join(", ")));
        }
        if let Some(e) = first.body_error {
            return Err(e);
        }
        let mut spec = spec.clone();
        let mut exec = |p: &model::Prepared| net::execute(&self.agent, &self.auth, &self.creds, p);
        let notes = crate::subst::apply_spec(&mut spec, &self.rules, &mut |r, v| self.cache.resolve(r, v, &mut exec))?;
        let mut prepared = if notes.is_empty() { first } else { model::prepare(&spec, method(&spec)) };
        // The scripts: before (it can change the request and the variables), then on the answer.
        let mut report = crate::script::ScriptReport::default();
        let (req, skip) = crate::script::before(&spec.pre_script, script_request(&prepared), &mut report);
        if skip {
            return Err("Skipped by the pre-request script (pm.execution.skipRequest)".into());
        }
        if let Some(e) = report.errors.first() {
            return Err(e.clone());
        }
        apply_request(&mut prepared, &req);
        let mut r = net::execute(&self.agent, &self.auth, &self.creds, &prepared)?;
        let resp = crate::script::ScriptResponse {
            status: r.status,
            status_text: r.status_text.clone(),
            headers: r.headers.clone(),
            body: r.body.clone(),
            time_ms: r.elapsed.as_millis() as u64,
        };
        crate::script::after(&spec.post_script, req, resp, &mut report);
        r.notes = notes;
        r.script = (!report.is_empty()).then_some(report);
        Ok(r)
    }
}

/// What a script sees of a request.
pub fn script_request(p: &model::Prepared) -> crate::script::ScriptRequest {
    crate::script::ScriptRequest { method: p.verb.clone(), url: p.url.clone(), headers: p.headers.clone(), body: p.body.clone().unwrap_or_default() }
}

/// What a pre-request script changed, back into the request.
pub fn apply_request(p: &mut model::Prepared, r: &crate::script::ScriptRequest) {
    if r.url != p.url {
        p.url = r.url.clone();
        p.display_url = r.url.clone();
    }
    p.verb = r.method.trim().to_uppercase();
    p.headers = r.headers.clone();
    if p.body.as_deref().unwrap_or_default() != r.body {
        p.body = (!r.body.is_empty()).then(|| r.body.clone());
    }
}

/// A request or flow to test, with its place in the workspace (`Folder/Sub/Name`).
#[derive(Clone)]
pub struct Item {
    pub path: String,
    pub node: Node,
}

/// The requests and flows under `node`, in the sidebar's order.
pub fn collect(node: &Node, prefix: &str) -> Vec<Item> {
    let mut out = Vec::new();
    // Requests whose auth is Inherit get their nearest folder's; without a profile, their folders'.
    fn walk(f: &Folder, path: &str, inherited: &crate::auth::Auth, profile: &str, out: &mut Vec<Item>) {
        let auth = f.auth.or(inherited).clone();
        let profile = if f.profile.is_empty() { profile } else { f.profile.as_str() };
        for n in &f.items {
            match n {
                Node::Folder(sub) => walk(sub, &format!("{path}{}/", sub.name), &auth, profile, out),
                Node::Request(r) if (r.spec.auth.is_inherit() && !auth.is_inherit()) || (r.spec.profile.is_empty() && !profile.is_empty()) => {
                    let mut r = r.clone();
                    if r.spec.auth.is_inherit() {
                        r.spec.auth = auth.clone();
                    }
                    if r.spec.profile.is_empty() {
                        r.spec.profile = profile.to_owned();
                    }
                    out.push(Item { path: format!("{path}{}", r.name), node: Node::Request(r) });
                }
                other => out.push(Item { path: format!("{path}{}", other.name()), node: other.clone() }),
            }
        }
    }
    match node {
        Node::Folder(f) => walk(f, prefix, &crate::auth::Auth::Inherit, "", &mut out),
        other => out.push(Item { path: format!("{prefix}{}", other.name()), node: other.clone() }),
    }
    out
}

#[derive(Clone)]
pub struct TestResult {
    pub path: String,
    /// `GET /v1/users` or `flow`.
    pub what: String,
    pub status: Option<u16>,
    pub duration: Duration,
    pub outcomes: Vec<Outcome>,
    /// Network error, path not filled, flow failure…
    pub error: Option<String>,
    pub passed: bool,
}

pub fn request_result(path: &str, spec: &RequestSpec, result: Result<ResponseData, String>, duration: Duration) -> TestResult {
    let what = format!("{} {}", spec.verb.trim().to_uppercase(), spec.path.trim());
    match result {
        Err(e) => TestResult { path: path.into(), what, status: None, duration, outcomes: Vec::new(), error: Some(e), passed: false },
        Ok(r) => {
            let mut outcomes = assert::check(&spec.assertions, &assert::context(&r), &|v| crate::dates::expand(v));
            // The tests of the post-response script (`pm.test`) count like assertions.
            let script_errors: Vec<String> = r.script.as_ref().map(|s| s.errors.clone()).unwrap_or_default();
            if let Some(rep) = &r.script {
                outcomes.extend(rep.tests.iter().map(|t| Outcome {
                    text: format!("pm.test “{}”", t.name),
                    passed: t.passed,
                    detail: t.error.clone().unwrap_or_default(),
                }));
            }
            let passed = script_errors.is_empty() && if outcomes.is_empty() { r.status < 400 } else { outcomes.iter().all(|o| o.passed) };
            let error = if let Some(e) = script_errors.first() {
                Some(e.clone())
            } else {
                (outcomes.is_empty() && r.status >= 400).then(|| format!("HTTP {} {} (add assertions to expect it)", r.status, r.status_text))
            };
            TestResult { path: path.into(), what, status: Some(r.status), duration, outcomes, error, passed }
        }
    }
}

/// Runs `items` in order, `delay` apart (the API's rate limit); `emit` gets each result as it comes.
pub fn run(items: &[Item], sender: &Sender, settings: &RunSettings, stop: &AtomicBool, mut emit: impl FnMut(TestResult)) {
    let mut last: Option<Instant> = None;
    for item in items {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Some(t) = last {
            if let Some(wait) = settings.delay.checked_sub(t.elapsed()) {
                std::thread::sleep(wait);
            }
        }
        last = Some(Instant::now());
        let started = Instant::now();
        let result = match &item.node {
            Node::Request(SavedRequest { spec, .. }) => request_result(&item.path, spec, sender.send(spec), started.elapsed()),
            Node::Flow(f) => run_flow(&item.path, f, sender, settings, stop),
            Node::Folder(_) => continue,
        };
        emit(result);
    }
}

fn run_flow(path: &str, f: &Flow, sender: &Sender, settings: &RunSettings, stop: &AtomicBool) -> TestResult {
    let started = Instant::now();
    let handle = flow::start(f.clone(), sender.agent.clone(), sender.auth.clone(), sender.creds.clone(), settings.clone(), || {});
    let (mut ok, mut errors, mut outcomes) = (false, Vec::new(), Vec::new());
    for ev in handle.rx.iter() {
        if stop.load(Ordering::Relaxed) {
            handle.stop.store(true, Ordering::Relaxed);
        }
        match ev {
            RunEvent::Log(l) if l.kind == LogKind::Error => errors.push(l.text),
            RunEvent::Tests(o) => outcomes.extend(o),
            RunEvent::Finished { ok: done, .. } => ok = done,
            _ => {}
        }
    }
    let error = (!ok).then(|| errors.last().map(|e| e.trim_start_matches("✖ ").to_owned()).unwrap_or_else(|| "failed".into()));
    TestResult { path: path.into(), what: "flow".into(), status: None, duration: started.elapsed(), outcomes, error, passed: ok }
}

/// (requests / flows passed, total, assertions passed, assertions total)
pub fn totals(results: &[TestResult]) -> (usize, usize, usize, usize) {
    let passed = results.iter().filter(|r| r.passed).count();
    let asserts: usize = results.iter().map(|r| r.outcomes.len()).sum();
    let asserts_ok: usize = results.iter().map(|r| r.outcomes.iter().filter(|o| o.passed).count()).sum();
    (passed, results.len(), asserts_ok, asserts)
}

/// JUnit XML (CI systems show it): one test case per request / flow, its failed assertions as the failure.
pub fn junit(name: &str, results: &[TestResult], total: Duration) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;");
    let failures = results.iter().filter(|r| !r.passed).count();
    let mut out = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites name=\"{}\" tests=\"{}\" failures=\"{failures}\" time=\"{:.3}\">\n  <testsuite name=\"{}\" tests=\"{}\" failures=\"{failures}\" time=\"{:.3}\">\n",
        esc(name),
        results.len(),
        total.as_secs_f64(),
        esc(name),
        results.len(),
        total.as_secs_f64()
    );
    for r in results {
        out.push_str(&format!("    <testcase classname=\"{}\" name=\"{}\" time=\"{:.3}\"", esc(&r.what), esc(&r.path), r.duration.as_secs_f64()));
        if r.passed {
            out.push_str(" />\n");
            continue;
        }
        let mut lines: Vec<String> = r.outcomes.iter().filter(|o| !o.passed).map(|o| format!("{} — {}", o.text, o.detail)).collect();
        if let Some(e) = &r.error {
            lines.insert(0, e.clone());
        }
        let message = lines.first().cloned().unwrap_or_default();
        out.push_str(&format!(">\n      <failure message=\"{}\">{}</failure>\n    </testcase>\n", esc(&message), esc(&lines.join("\n"))));
    }
    out.push_str("  </testsuite>\n</testsuites>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assert::{Assertion, Op};

    fn resp(status: u16, body: &str) -> ResponseData {
        ResponseData {
            status,
            status_text: "X".into(),
            elapsed: Duration::from_millis(5),
            size: body.len(),
            headers: Vec::new(),
            body: body.into(),
            is_json: true,
            notes: Vec::new(),
            script: None,
        }
    }

    #[test]
    fn requests_pass_or_fail() {
        let mut spec = RequestSpec::default();
        // No assertion: the status decides.
        assert!(request_result("a", &spec, Ok(resp(200, "[]")), Duration::ZERO).passed);
        assert!(!request_result("a", &spec, Ok(resp(404, "{}")), Duration::ZERO).passed);
        assert!(!request_result("a", &spec, Err("offline".into()), Duration::ZERO).passed);
        // Assertions decide: a 404 can be what is expected.
        spec.assertions = vec![Assertion::new("res.status", Op::Eq, "404")];
        assert!(request_result("a", &spec, Ok(resp(404, "{}")), Duration::ZERO).passed);
        spec.assertions.push(Assertion::new("res.body.error", Op::IsDefined, ""));
        let r = request_result("Folder/a", &spec, Ok(resp(404, "{}")), Duration::ZERO);
        assert!(!r.passed);
        assert_eq!(totals(std::slice::from_ref(&r)), (0, 1, 1, 2));
        let xml = junit("ws", &[r], Duration::from_secs(1));
        assert!(xml.contains(r#"<testcase classname="GET /" name="Folder/a""#), "{xml}");
        assert!(xml.contains("res.body.error isDefined — the field is missing"), "{xml}");
    }

    #[test]
    fn collects_in_order() {
        let mut root = Folder::new("root");
        let mut sub = Folder::new("Sub");
        sub.items.push(Node::Request(SavedRequest { uid: 1, name: "b".into(), spec: RequestSpec::default() }));
        root.items.push(Node::Request(SavedRequest { uid: 2, name: "a".into(), spec: RequestSpec::default() }));
        root.items.push(Node::Folder(sub));
        root.items.push(Node::Flow(Flow::new("f")));
        let items = collect(&Node::Folder(root), "");
        assert_eq!(items.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(), ["a", "Sub/b", "f"]);
    }
}
