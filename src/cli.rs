//! Command line, to run and check workspace files without the GUI (handy for scripts and AI agents).

use std::sync::Arc;
use std::time::Duration;

use crate::docs::{ApiDoc, Param};
use crate::flow::{self, BlockKind, Flow, LogKind, RunEvent, RunSettings};
use crate::model;
use crate::net::{self, Credentials};
use crate::storage::{self, Config};
use crate::workspace::{Folder, Node};
use crate::workspace_fs;
use parking_lot::Mutex;

/// `println!` that quits quietly when the reader went away (`… | head`).
macro_rules! out {
    ($($t:tt)*) => {{
        use std::io::Write;
        if writeln!(std::io::stdout(), $($t)*).is_err() {
            std::process::exit(0);
        }
    }};
}

const HELP: &str = "apifa — API For All

Usage:
  apifa                          open the app
  apifa run <flow> [-i name=value]... [--vars] [--quiet] [--resume] [--mock] [--env name]
                                         run a flow (name, file path or uid) and print its console
                                         (--resume: go on with its stopped run; --mock: against the mock server;
                                         --env: an environment's {{values}}, default the app's active one, none = no environment)
  apifa test [target]... [--junit file.xml] [--quiet] [--mock] [--env name]
                                         send saved requests and run flows, check their assertions
                                         (target: a folder, a request / flow name or file; none = all)
  apifa understand [--mock] [--env name] [--saved-only]
                                         send the saved GET requests, then every documented GET (the ids a GET
                                         returns unlock the ones that need them), and map the structure of the
                                         data (entities, fields, links; no value kept): DATA_MAP.md
                                         (--saved-only: only the saved GET requests)
  apifa datamap [-f json|md|html] [-o file]
                                         export the data map (default: Markdown on the standard output)
  apifa datamap --load file.json make an exported data map the current one
  apifa mock [--port 7643]       run the mock server (a local stand-in for the API) until Ctrl+C
  apifa docs [target]... [--format html|md|openapi] [-o file] [--title t] [--no-examples]
                                         documentation of saved requests and flows (none = all)
  apifa docs --api [resource]... [--format …] [-o file]
                                         documentation of the imported APIs (resources: users, pets…; none = all)
  apifa check [file]             validate the workspace files (or one file)
  apifa list                     list saved requests and flows
  apifa endpoints [words]...     search the imported API documentation
  apifa path                     print the workspace folder
  apifa help

The workspace format is described in AGENTS.md (in the workspace folder).";

trait Also {
    fn also(self, f: fn()) -> Self;
}

impl Also for i32 {
    fn also(self, f: fn()) -> i32 {
        f();
        self
    }
}

/// Runs a command; `None` when there are no arguments (start the GUI).
pub fn main(args: &[String]) -> Option<i32> {
    // `--profile name`, anywhere: the API profile of this command (else the active one).
    let mut args = args.to_vec();
    let mut profiles = crate::profiles::Profiles::load();
    if let Some(i) = args.iter().position(|a| a == "--profile") {
        args.remove(i);
        let Some(name) = (i < args.len()).then(|| args.remove(i)) else {
            eprintln!("--profile expects a profile name or id (apifa profiles lists them)");
            return Some(2);
        };
        match profiles.find(&name).map(|p| p.id.clone()) {
            Some(id) => profiles.active = Some(id),
            None => {
                eprintln!("No profile “{name}” (apifa profiles lists them)");
                return Some(2);
            }
        }
    }
    crate::profiles::set_current(&profiles);
    let (cmd, rest) = args.split_first()?;
    Some(match cmd.as_str() {
        "run" => run(rest).also(save_env_changes),
        "test" => test(rest).also(save_env_changes),
        "understand" => understand(rest),
        "datamap" => datamap_cmd(rest),
        "import-api" => import_api(rest),
        "apis" => apis_cmd(rest),
        "profiles" => {
            if profiles.list.is_empty() {
                out!("No API profile yet: importing an API definition makes one (apifa import-api …)");
            }
            for p in &profiles.list {
                let active = if profiles.active.as_deref() == Some(p.id.as_str()) { " (active)" } else { "" };
                out!(
                    "{}{active}  — id {} · {} · auth {} · APIs: {}",
                    p.name,
                    p.id,
                    if p.base_url.is_empty() { "no base URL" } else { p.base_url.as_str() },
                    p.auth.label(),
                    p.apis.join(", ")
                );
            }
            0
        }
        "mock" => mock(rest),
        "docs" => docs_cmd(rest),
        "check" => check(rest),
        "list" => list(),
        "endpoints" => endpoints(rest),
        "path" => {
            out!("{}", workspace_fs::dir().display());
            0
        }
        "help" | "-h" | "--help" => {
            out!("{HELP}");
            0
        }
        other => {
            eprintln!("Unknown command “{other}”.\n\n{HELP}");
            2
        }
    })
}

fn workspace() -> Result<Folder, String> {
    let loaded = workspace_fs::load().ok_or("No workspace yet: open the app once to create it.")?;
    for p in &loaded.problems {
        eprintln!("warning: unreadable file — {p}");
    }
    Ok(loaded.root)
}

fn all_nodes<'a>(f: &'a Folder, path: &str, out: &mut Vec<(String, &'a Node)>) {
    for n in &f.items {
        match n {
            Node::Folder(sub) => all_nodes(sub, &format!("{path}{}/", sub.name), out),
            other => out.push((format!("{path}{}", other.name()), other)),
        }
    }
}

fn find_flow(arg: &str) -> Result<Flow, String> {
    let path = std::path::Path::new(arg);
    if path.is_file() {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{arg}: invalid flow: {e}"))?;
        let has_uid = v.get("uid").is_some_and(serde_json::Value::is_u64);
        let mut flow: Flow = serde_json::from_value(v).map_err(|e| format!("{arg}: invalid flow: {e}"))?;
        // Without a uid each run would get a new one: --resume and kept results could never find it again.
        // One made from the file's path (FNV-1a: the same from one build to the next) stays the same from run to run.
        if !has_uid {
            let full = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
            let mut h: u64 = 0xcbf29ce484222325;
            for b in full.to_string_lossy().bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            flow.uid = (h >> 11).max(1);
        }
        return Ok(flow);
    }
    let mut loaded = workspace_fs::load().ok_or("No workspace yet: open the app once to create it.")?;
    for p in &loaded.problems {
        eprintln!("warning: unreadable file — {p}");
    }
    // Files written by hand get their uid now, like the app does: kept results stay linked to the flow.
    if loaded.needs_save {
        let _ = workspace_fs::save(&loaded.root, &mut loaded.index);
    }
    let root = loaded.root;
    let mut nodes = Vec::new();
    all_nodes(&root, "", &mut nodes);
    let flows: Vec<(&String, &Flow)> = nodes
        .iter()
        .filter_map(|(p, n)| match n {
            Node::Flow(f) => Some((p, f)),
            _ => None,
        })
        .collect();
    let lower = arg.to_lowercase();
    let exact: Vec<_> = flows.iter().filter(|(p, f)| f.name.to_lowercase() == lower || p.to_lowercase() == lower || f.uid.to_string() == arg).collect();
    let candidates: Vec<_> = if exact.is_empty() { flows.iter().filter(|(p, _)| p.to_lowercase().contains(&lower)).collect() } else { exact };
    match candidates.as_slice() {
        [(_, f)] => Ok((*f).clone()),
        [] => Err(format!("No flow matches “{arg}”. See `apifa list`.")),
        many => Err(format!("“{arg}” matches several flows:\n{}", many.iter().map(|(p, _)| format!("  {p}")).collect::<Vec<_>>().join("\n"))),
    }
}

fn run(args: &[String]) -> i32 {
    let mut target = None;
    let mut inputs = Vec::new();
    let (mut show_vars, mut quiet, mut resume, mut mock) = (false, false, false, false);
    let mut env = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-i" | "--input" => match it.next().and_then(|kv| kv.split_once('=')) {
                Some((k, v)) => inputs.push((k.to_owned(), v.to_owned())),
                None => {
                    eprintln!("-i expects name=value");
                    return 2;
                }
            },
            "--vars" => show_vars = true,
            "--quiet" | "-q" => quiet = true,
            "--resume" => resume = true,
            "--mock" => mock = true,
            "--env" => match it.next() {
                Some(e) => env = Some(e.clone()),
                None => {
                    eprintln!("--env expects an environment name (or none)");
                    return 2;
                }
            },
            other => target = Some(other.to_owned()),
        }
    }
    let Some(target) = target else {
        eprintln!("Usage: apifa run <flow> [-i name=value]... [--vars] [--quiet] [--resume] [--mock] [--env name]");
        return 2;
    };
    if let Err(e) = use_env(env.as_deref()) {
        eprintln!("{e}");
        return 1;
    }
    let mut flow = match find_flow(&target) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    for (k, v) in inputs {
        match flow.inputs.iter_mut().find(|i| i.name == k) {
            Some(i) => i.value = v,
            None => flow.inputs.push(flow::FlowInput { name: k, value: v, ..Default::default() }),
        }
    }
    let _mock = match mock.then(start_mock).transpose() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let config: Config = crate::secrets::load_config();
    crate::model::set_default_base(&config.base_url);
    crate::auth::set_default(config.auth.clone());
    let doc = Arc::new(crate::docs::ApiDoc::load().unwrap_or_else(|_| crate::docs::ApiDoc::empty()));
    let subs = Arc::new(crate::subst::Substitutions::load(&doc).rules());
    let settings = RunSettings {
        delay: Duration::from_millis(config.flow_delay_ms),
        parallel: config.parallel,
        parallel_rate: config.parallel_rate,
        max_pages: config.max_pages,
        doc,
        flows: Arc::new(library().to_vec()),
        subs,
        subs_cache: Arc::default(),
        checkpoint: None,
        stop_after: None,
        profile: String::new(),
    };
    let mut settings = settings;
    // Responses are kept as they arrive: a run stopped (Ctrl+C, error, hourly limit) goes on with --resume.
    match crate::resume::prepare(&mut flow, resume) {
        Ok(cp) => settings.checkpoint = Some(Arc::new(cp)),
        Err(e) if resume => {
            eprintln!("{e}");
            return 1;
        }
        Err(e) => eprintln!("(this run can't be resumed: {e})"),
    }
    let creds = Credentials { auth: config.auth.clone() };
    let auth = net::SharedAuth::default();
    let (uid, name, keep) = (flow.uid, flow.name.clone(), flow.keep_results);
    let started = std::time::Instant::now();
    let handle = flow::start(flow, net::agent(), auth, creds, settings, || {});
    let mut ok = false;
    for ev in handle.rx.iter() {
        match ev {
            RunEvent::Log(l) => {
                if quiet && !matches!(l.kind, LogKind::Error | LogKind::Warning) {
                    continue;
                }
                let line = format!("{}{}", "  ".repeat(l.depth), l.text);
                if matches!(l.kind, LogKind::Error | LogKind::Warning) {
                    eprintln!("{line}");
                } else {
                    out!("{line}");
                }
            }
            RunEvent::Rate(_) | RunEvent::Progress(_) | RunEvent::Tests(_) => {}
            RunEvent::Finished { ok: done, vars } => {
                ok = done;
                if show_vars {
                    out!("{}", serde_json::to_string_pretty(&vars).unwrap_or_default());
                }
                // "Keep results": the app shows them when the flow is opened.
                if keep {
                    let run = crate::runs::SavedRun { flow: name.clone(), ok, at: storage::now(), duration_ms: started.elapsed().as_millis() as u64, vars };
                    if let Err(e) = crate::runs::save(uid, &run) {
                        eprintln!("couldn't keep the results: {e}");
                    }
                }
            }
        }
    }
    if !ok && crate::resume::path(uid).exists() {
        eprintln!("Go on from here: apifa run {} --resume", shell_quote(&target));
    }
    if ok {
        0
    } else {
        1
    }
}

/// Problems a flow would hit at run time, found statically.
/// The workspace's flows (empty when there is no workspace yet).
fn library() -> &'static [Flow] {
    static FLOWS: std::sync::OnceLock<Vec<Flow>> = std::sync::OnceLock::new();
    FLOWS.get_or_init(|| workspace_fs::load().map(|l| l.root.flows().into_iter().cloned().collect()).unwrap_or_default())
}

fn docs() -> &'static crate::docs::ApiDoc {
    static DOC: std::sync::OnceLock<crate::docs::ApiDoc> = std::sync::OnceLock::new();
    DOC.get_or_init(|| crate::docs::ApiDoc::load().unwrap_or_else(|_| crate::docs::ApiDoc::empty()))
}

pub fn lint_flow(flow: &Flow, out: &mut Vec<String>) {
    fn walk(blocks: &[flow::Block], at: &str, out: &mut Vec<String>) {
        for (i, b) in blocks.iter().enumerate() {
            let here = format!("{at}block {}", i + 1);
            match &b.kind {
                BlockKind::Request(r) => {
                    if !model::VERBS.contains(&r.verb.trim().to_uppercase().as_str()) {
                        out.push(format!("{here}: unknown verb “{}”", r.verb));
                    }
                    if r.path.trim().is_empty() {
                        out.push(format!("{here}: empty path"));
                    }
                    if r.save_as.trim().is_empty() {
                        out.push(format!("{here}: empty save_as"));
                    }
                    crate::assert::lint(&r.assertions, &format!("{here}: "), out);
                    if !r.body.trim().is_empty() {
                        // Placeholders replaced by a dummy value: the rest must be valid JSON.
                        let probe = replace_placeholders(&r.body);
                        if let Err(e) = serde_json::from_str::<serde_json::Value>(&probe) {
                            out.push(format!("{here}: body is not valid JSON ({e})"));
                        }
                    }
                    if !r.values.is_empty() && r.doc.is_none() && docs().match_route(&r.verb, &r.path).is_none() {
                        out.push(format!("{here}: “values” are ignored: {} {} isn't a documented endpoint", r.verb, r.path));
                    }
                    for f in r.files.iter().filter(|f| f.enabled && !(f.key.trim().is_empty() && f.value.trim().is_empty())) {
                        let (key, path) = (f.key.trim(), f.value.trim());
                        if key.is_empty() || path.is_empty() {
                            out.push(format!("{here}: a file needs a field name (key) and a path (value)"));
                        } else if !model::has_body(&r.verb.trim().to_uppercase()) {
                            out.push(format!("{here}: files are only sent with POST, PUT or PATCH"));
                        } else if !path.contains("{{") && !model::expand_home(path).is_file() {
                            out.push(format!("{here}: file not found for {key}: {path}"));
                        }
                    }
                }
                BlockKind::Flow(c) => match flow::find_flow(library().iter(), c.flow_uid, &c.flow) {
                    _ if c.flow.trim().is_empty() && c.flow_uid.is_none() => out.push(format!("{here}: “flow” is empty")),
                    None => out.push(format!("{here}: no flow named “{}” in the workspace", c.flow.trim())),
                    Some(sub) => {
                        for k in c.inputs.keys().filter(|k| !sub.inputs.iter().any(|i| i.name.trim() == k.as_str())) {
                            out.push(format!("{here}: “{}” has no input named “{k}”", sub.name));
                        }
                    }
                },
                BlockKind::Extract { from, into } if from.trim().is_empty() || into.trim().is_empty() => {
                    out.push(format!("{here}: extract needs “from” and “into”"));
                }
                BlockKind::ForEach { list, item, blocks, .. } => {
                    if list.trim().is_empty() || item.trim().is_empty() {
                        out.push(format!("{here}: for_each needs “list” and “item”"));
                    }
                    walk(blocks, &format!("{here} › "), out);
                }
                BlockKind::If { then_blocks, else_blocks, .. } => {
                    walk(then_blocks, &format!("{here} › then › "), out);
                    walk(else_blocks, &format!("{here} › else › "), out);
                }
                _ => {}
            }
        }
    }
    for input in &flow.inputs {
        if input.name.trim().is_empty() {
            out.push("an input has no name".into());
        } else if input.kind == flow::InputType::Enum && input.options.is_empty() {
            out.push(format!("input “{}”: enum without options", input.name.trim()));
        } else if let Err(e) = input.to_value() {
            out.push(e);
        }
    }
    if flow.outputs.iter().any(|o| o.name.trim().is_empty()) {
        out.push("an output has no name".into());
    }
    walk(&flow.blocks, "", out);
}

fn replace_placeholders(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(s) = rest.find("{{") {
        out.push_str(&rest[..s]);
        match rest[s..].find("}}") {
            Some(e) => {
                out.push('0');
                rest = &rest[s + e + 2..];
            }
            None => {
                rest = &rest[s..];
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn check(args: &[String]) -> i32 {
    let mut problems = Vec::new();
    let mut checked = 0;
    if let Some(file) = args.first() {
        let text = match std::fs::read_to_string(file) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{file}: {e}");
                return 1;
            }
        };
        checked = 1;
        if file.ends_with(workspace_fs::FLOW_EXT) {
            match serde_json::from_str::<Flow>(&text) {
                Ok(f) => lint_flow(&f, &mut problems),
                Err(e) => problems.push(format!("invalid flow: {e}")),
            }
        } else {
            match serde_json::from_str::<crate::workspace::SavedRequest>(&text) {
                Ok(r) => crate::assert::lint(&r.spec.assertions, "", &mut problems),
                Err(e) => problems.push(format!("invalid request: {e}")),
            }
        }
        problems.iter_mut().for_each(|p| *p = format!("{file}: {p}"));
    } else {
        let Some(loaded) = workspace_fs::load() else {
            eprintln!("No workspace yet: open the app once to create it.");
            return 1;
        };
        problems.extend(loaded.problems);
        let mut nodes = Vec::new();
        all_nodes(&loaded.root, "", &mut nodes);
        for (path, n) in nodes {
            checked += 1;
            let mut own = Vec::new();
            match n {
                Node::Flow(f) => lint_flow(f, &mut own),
                Node::Request(r) => crate::assert::lint(&r.spec.assertions, "", &mut own),
                Node::Folder(_) => {}
            }
            problems.extend(own.into_iter().map(|p| format!("{path}: {p}")));
        }
    }
    if problems.is_empty() {
        out!("OK — {checked} item(s) checked");
        0
    } else {
        for p in &problems {
            eprintln!("✗ {p}");
        }
        1
    }
}

fn list() -> i32 {
    let root = match workspace() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let mut nodes = Vec::new();
    all_nodes(&root, "", &mut nodes);
    for (path, n) in nodes {
        match n {
            Node::Request(r) => out!("request  {:<7} {path}   ({})", r.spec.verb, r.spec.path),
            Node::Flow(f) => out!("flow     {:<7} {path}   ({} blocks)", "", f.blocks.len()),
            Node::Folder(_) => {}
        }
    }
    0
}

fn endpoints(words: &[String]) -> i32 {
    let doc = match ApiDoc::load() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let terms: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
    fn leaves(params: &[&Param], req: &mut Vec<String>, opt: &mut Vec<String>) {
        for p in params {
            if p.is_group() {
                leaves(&p.children.iter().collect::<Vec<_>>(), req, opt);
            } else if p.required {
                req.push(format!("{} ({})", p.full_name, p.kind.label()));
            } else {
                opt.push(p.full_name.clone());
            }
        }
    }
    let mut shown = 0;
    for hit in doc.search(&terms.join(" ")) {
        let res = &doc.resources[hit.resource];
        {
            for (mi, i) in hit.routes {
                let m = &res.methods[mi];
                let r = &m.routes[i];
                shown += 1;
                if shown > 60 {
                    continue;
                }
                out!("{:<6} {}   [doc: {{\"resource\":\"{}\",\"method\":\"{}\",\"route\":{i}}}]", r.verb, r.path, res.key, m.name);
                let (mut req, mut opt) = (Vec::new(), Vec::new());
                leaves(&model::field_params(m, &r.path), &mut req, &mut opt);
                if !req.is_empty() {
                    out!("       required: {}", req.join(", "));
                }
                if !opt.is_empty() {
                    let more = if opt.len() > 15 { format!(" … (+{})", opt.len() - 15) } else { String::new() };
                    out!("       optional: {}{more}", opt.iter().take(15).cloned().collect::<Vec<_>>().join(", "));
                }
            }
        }
    }
    if shown > 60 {
        out!("… {} more, add words to narrow the search", shown - 60);
    }
    if shown == 0 {
        out!("No endpoint matches.");
    }
    0
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `test`: the test runner (requests + flows), like Bruno's `bru run`.
fn test(args: &[String]) -> i32 {
    let (mut targets, mut junit, mut quiet, mut mock) = (Vec::new(), None, false, false);
    let mut env: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--junit" => match it.next() {
                Some(f) => junit = Some(f.clone()),
                None => {
                    eprintln!("--junit expects a file name");
                    return 2;
                }
            },
            "--quiet" | "-q" => quiet = true,
            "--mock" => mock = true,
            "--env" => match it.next() {
                Some(e) => env = Some(e.clone()),
                None => {
                    eprintln!("--env expects an environment name (or none)");
                    return 2;
                }
            },
            other => targets.push(other.to_owned()),
        }
    }
    if let Err(e) = use_env(env.as_deref()) {
        eprintln!("{e}");
        return 1;
    }
    let items = match select(&targets) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let _mock = match mock.then(start_mock).transpose() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let config: Config = crate::secrets::load_config();
    crate::model::set_default_base(&config.base_url);
    crate::auth::set_default(config.auth.clone());
    let doc = Arc::new(ApiDoc::load().unwrap_or_else(|_| ApiDoc::empty()));
    let rules = Arc::new(crate::subst::Substitutions::load(&doc).rules());
    let sender = crate::testrun::Sender {
        agent: net::agent(),
        auth: net::SharedAuth::default(),
        creds: Credentials { auth: config.auth.clone() },
        doc: doc.clone(),
        rules: rules.clone(),
        cache: Arc::default(),
    };
    let settings = RunSettings {
        delay: Duration::from_millis(config.flow_delay_ms),
        parallel: config.parallel,
        parallel_rate: config.parallel_rate,
        max_pages: config.max_pages,
        doc,
        flows: Arc::new(library().to_vec()),
        subs: rules,
        subs_cache: sender.cache.clone(),
        checkpoint: None,
        stop_after: None,
        profile: String::new(),
    };
    let started = std::time::Instant::now();
    let mut results = Vec::new();
    let stop = std::sync::atomic::AtomicBool::new(false);
    crate::testrun::run(&items, &sender, &settings, &stop, |r| {
        let status = r.status.map(|s| format!(" · {s}")).unwrap_or_default();
        let asserts = if r.outcomes.is_empty() { String::new() } else { format!(" · {}", crate::assert::summary(&r.outcomes)) };
        let line = format!("{} {}   {}{status} · {} ms{asserts}", if r.passed { "✓" } else { "✗" }, r.path, r.what, r.duration.as_millis());
        if r.passed {
            if !quiet {
                out!("{line}");
            }
        } else {
            out!("{line}");
            if let Some(e) = &r.error {
                out!("    {e}");
            }
            for o in r.outcomes.iter().filter(|o| !o.passed) {
                out!("    ✗ {} — {}", o.text, o.detail);
            }
        }
        results.push(r);
    });
    let (passed, total, a_ok, a_total) = crate::testrun::totals(&results);
    let asserts = if a_total > 0 { format!(" · assertions {a_ok}/{a_total}") } else { String::new() };
    out!("\n{passed} passed, {} failed ({total}){asserts} · {:.1} s", total - passed, started.elapsed().as_secs_f64());
    if let Some(file) = junit {
        let name = if targets.is_empty() { "workspace".to_owned() } else { targets.join(", ") };
        if let Err(e) = std::fs::write(&file, crate::testrun::junit(&name, &results, started.elapsed())) {
            eprintln!("can't write {file}: {e}");
            return 1;
        }
    }
    if passed == total {
        0
    } else {
        1
    }
}

/// `--mock`: a mock server on a free port, and every request of this command sent to it.
/// `understand`: every saved GET request sent, the structure of what came back mapped (datamap.rs).
/// `import-api <file | URL> [--name n]`: an OpenAPI / Swagger / apipie definition.
fn import_api(args: &[String]) -> i32 {
    let (mut src, mut name) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--name" => name = it.next().cloned(),
            other if src.is_none() => src = Some(other.to_owned()),
            other => {
                eprintln!("Unknown option “{other}”. Usage: apifa import-api <file | URL> [--name n]");
                return 2;
            }
        }
    }
    let Some(src) = src else {
        eprintln!("Usage: apifa import-api <file | URL> [--name n]");
        return 2;
    };
    let text = if src.starts_with("http://") || src.starts_with("https://") {
        match net::agent().get(&src).call().map_err(|e| e.to_string()).and_then(|mut r| r.body_mut().read_to_string().map_err(|e| e.to_string())) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Can't fetch {src}: {e}");
                return 1;
            }
        }
    } else {
        match std::fs::read_to_string(&src) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("Can't read {src}: {e}");
                return 1;
            }
        }
    };
    match crate::apis::import(&text, &src, name.as_deref()) {
        Ok((e, parsed)) => {
            let mut ps = crate::profiles::Profiles::load();
            let id = crate::profiles::profile_for_import(&mut ps, &e, parsed.auth);
            if ps.active.is_none() {
                ps.active = Some(id.clone());
            }
            ps.save();
            out!("Profile “{id}”: its base URL and auth are in profiles.json (or the app: Edit API profiles…)");
            out!(
                "Imported “{}” ({}, {} endpoints){}",
                e.name,
                e.format,
                e.endpoints,
                if e.base_urls.is_empty() { String::new() } else { format!(" — servers: {}", e.base_urls.join(", ")) }
            );
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// `apis` lists the imported definitions; `apis --remove name`.
fn apis_cmd(args: &[String]) -> i32 {
    if let [flag, name] = args {
        if flag == "--remove" {
            return match crate::apis::remove(name) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            };
        }
    }
    let list = crate::apis::list();
    if list.is_empty() {
        out!("No API imported yet: apifa import-api <openapi.json | URL>");
    }
    for e in list {
        out!(
            "{}{}  — {}, {} endpoints{}",
            e.name,
            if e.enabled { "" } else { " (off)" },
            e.format,
            e.endpoints,
            if e.source.is_empty() { String::new() } else { format!(" · {}", e.source) }
        );
    }
    0
}

fn datamap_cmd(args: &[String]) -> i32 {
    use crate::datamap::Export;
    const USAGE: &str = "Usage: apifa datamap [-f json|md|html] [-o file] | --load file.json";
    let (mut format, mut out, mut load) = (None, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |what: &str| it.next().cloned().ok_or_else(|| format!("{a} expects {what}. {USAGE}"));
        let r = match a.as_str() {
            "-f" | "--format" => value("a format").and_then(|f| Export::from_name(&f).map(|f| format = Some(f)).ok_or_else(|| format!("Unknown format “{f}”. {USAGE}"))),
            "-o" | "--output" => value("a file").map(|f| out = Some(f)),
            "--load" => value("a file").map(|f| load = Some(f)),
            other => Err(format!("Unknown option “{other}”. {USAGE}")),
        };
        if let Err(e) = r {
            eprintln!("{e}");
            return 2;
        }
    }
    if let Some(file) = load {
        return match std::fs::read_to_string(&file).map_err(|e| e.to_string()).and_then(|t| crate::datamap::parse(&t)) {
            Ok(map) => {
                crate::datamap::replace(&map);
                out!("Loaded: {} entities, {} links. It is the current data map (DATA_MAP.md updated).", map.entities.len(), map.links.len());
                0
            }
            Err(e) => {
                eprintln!("Can't load {file}: {e}");
                1
            }
        };
    }
    let Some(map) = crate::datamap::load() else {
        eprintln!("No data map yet: run apifa understand (or Understand in the app's Data map tab).");
        return 1;
    };
    // The format: -f, else the output file's extension, else Markdown.
    let format = format
        .or_else(|| out.as_deref().and_then(|o| std::path::Path::new(o).extension()).and_then(|e| Export::from_name(&e.to_string_lossy())))
        .unwrap_or(Export::Markdown);
    let text = crate::datamap::render(&map, format);
    match out {
        Some(file) => match std::fs::write(&file, text) {
            Ok(()) => {
                out!("Data map written to {file}");
                0
            }
            Err(e) => {
                eprintln!("Can't write {file}: {e}");
                1
            }
        },
        None => {
            out!("{text}");
            0
        }
    }
}

fn understand(args: &[String]) -> i32 {
    let (mut mock, mut env, mut saved_only) = (false, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mock" => mock = true,
            "--saved-only" => saved_only = true,
            "--env" => match it.next() {
                Some(e) => env = Some(e.clone()),
                None => {
                    eprintln!("--env expects an environment name (or none)");
                    return 2;
                }
            },
            other => {
                eprintln!("Unknown option “{other}”. Usage: apifa understand [--mock] [--env name] [--saved-only]");
                return 2;
            }
        }
    }
    if let Err(e) = use_env(env.as_deref()) {
        eprintln!("{e}");
        return 1;
    }
    let root = match workspace() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let _mock = match mock.then(start_mock).transpose() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let config: Config = crate::secrets::load_config();
    crate::model::set_default_base(&config.base_url);
    crate::auth::set_default(config.auth.clone());
    let doc = Arc::new(ApiDoc::load().unwrap_or_else(|_| ApiDoc::empty()));
    let sender = crate::testrun::Sender {
        agent: net::agent(),
        auth: net::SharedAuth::default(),
        creds: Credentials { auth: config.auth.clone() },
        doc: doc.clone(),
        rules: Arc::new(crate::subst::Substitutions::load(&doc).rules()),
        cache: Arc::default(),
    };
    let profile = crate::profiles::active_id();
    let requests = crate::datamap::saved_gets(&root, &profile);
    let learned: crate::learned::Shared = Arc::new(Mutex::new(crate::learned::Examples::load()));
    let stop = std::sync::atomic::AtomicBool::new(false);
    let delay = Duration::from_millis(config.flow_delay_ms);
    let map = crate::datamap::understand(&sender, &requests, &learned, !saved_only, delay, &stop, &profile, |i, n, what, _| {
        if i < n {
            eprintln!("[{}/{n}] {what}", i + 1);
        }
    });
    crate::datamap::save(&map);
    let read = map.sources.iter().filter(|s| s.problem.is_none()).count();
    out!("{} entities, {} links, from {read} GET responses ({} not read).", map.entities.len(), map.links.len(), map.sources.len() - read);
    out!("Written: {}", workspace_fs::dir().join(crate::datamap::files(&profile).1).display());
    0
}

/// The environment whose `{{values}}` requests and flows use: `name`, else the app's active one.
/// `none`: no environment.
/// What scripts set in the environment during a command (`pm.environment.set`), saved into it.
fn save_env_changes() {
    let changes = crate::env::take_changes();
    if !changes.is_empty() {
        let mut envs = crate::env::Environments::load();
        if envs.apply_changes(&changes) {
            envs.save();
        }
    }
}

fn use_env(name: Option<&str>) -> Result<(), String> {
    let mut envs = crate::env::Environments::load();
    match name {
        Some(n) if n.eq_ignore_ascii_case("none") => envs.select(None)?,
        Some(n) => envs.select(Some(n))?,
        None => {}
    }
    if let Some(e) = envs.active() {
        eprintln!("(environment “{}”)", e.name);
    }
    crate::env::set_current(envs.active_vars());
    Ok(())
}

fn start_mock() -> Result<crate::mock::Server, String> {
    let doc = Arc::new(ApiDoc::load().unwrap_or_else(|_| ApiDoc::empty()));
    let server = crate::mock::start(0, Arc::new(crate::mock::Mocks::new(doc)), |_| {})?;
    net::set_mock_target(Some(server.url()));
    eprintln!("(against the mock server: nothing is sent to the real API)");
    Ok(server)
}

/// `mock`: the mock server in the foreground, one line per request.
fn mock(args: &[String]) -> i32 {
    let mut port = crate::mock::DEFAULT_PORT;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.next().map(|p| p.parse::<u16>())) {
            ("--port" | "-p", Some(Ok(p))) => port = p,
            _ => {
                eprintln!("Usage: apifa mock [--port 7643]");
                return 2;
            }
        }
    }
    let doc = Arc::new(ApiDoc::load().unwrap_or_else(|_| ApiDoc::empty()));
    let server = match crate::mock::start(port, Arc::new(crate::mock::Mocks::new(doc)), |line| out!("{line}")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    out!("Mock server on {} — use it as your base URL (Ctrl+C to stop)", server.url());
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// The requests and flows named on the command line: folders (`Users` covers `Users/…`), names, files; none = all.
fn select(targets: &[String]) -> Result<Vec<crate::testrun::Item>, String> {
    let root = workspace()?;
    let all = crate::testrun::collect(&Node::Folder(root), "");
    if targets.is_empty() {
        return Ok(all);
    }
    let mut items = Vec::new();
    for t in targets {
        let path = std::path::Path::new(t);
        if path.is_file() {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{t}: {e}"))?;
            let node = if t.ends_with(workspace_fs::FLOW_EXT) { serde_json::from_str(&text).map(Node::Flow) } else { serde_json::from_str(&text).map(Node::Request) };
            items.push(crate::testrun::Item { path: t.clone(), node: node.map_err(|e| format!("{t}: {e}"))? });
            continue;
        }
        let lower = t.trim_end_matches('/').to_lowercase();
        let found: Vec<_> = all
            .iter()
            .filter(|i| {
                let p = i.path.to_lowercase();
                p == lower || p.starts_with(&format!("{lower}/")) || i.node.name().to_lowercase() == lower
            })
            .cloned()
            .collect();
        if found.is_empty() {
            return Err(format!("Nothing matches “{t}”. See `apifa list`."));
        }
        items.extend(found);
    }
    Ok(items)
}

/// `docs`: documentation of the workspace (or some folders) as HTML, Markdown or OpenAPI.
fn docs_cmd(args: &[String]) -> i32 {
    let (mut targets, mut format, mut output, mut examples, mut title, mut api) = (Vec::new(), crate::docgen::Format::Html, None, true, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--format" | "-f" => match it.next().and_then(|f| crate::docgen::Format::parse(f)) {
                Some(f) => format = f,
                None => {
                    eprintln!("--format expects html, md or openapi");
                    return 2;
                }
            },
            "-o" | "--output" => output = it.next().cloned(),
            "--title" => title = it.next().cloned(),
            "--no-examples" => examples = false,
            "--api" => api = true,
            other => targets.push(other.to_owned()),
        }
    }
    if api {
        let title = title.unwrap_or_else(|| if targets.is_empty() { "API".to_owned() } else { format!("API — {}", targets.join(", ")) });
        let (text, n) = crate::docgen::generate_api(docs(), &targets, &crate::docgen::Options { title, examples, data_map: crate::datamap::load() }, format);
        if n == 0 {
            eprintln!("No resource matches {}. See `apifa endpoints`.", targets.join(", "));
            return 1;
        }
        return match output {
            Some(file) => match std::fs::write(&file, text) {
                Ok(()) => {
                    eprintln!("{n} endpoint(s) documented in {file}");
                    0
                }
                Err(e) => {
                    eprintln!("can't write {file}: {e}");
                    1
                }
            },
            None => {
                out!("{text}");
                0
            }
        };
    }
    let items = match select(&targets) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let title = title.unwrap_or_else(|| if targets.is_empty() { "apifa — workspace".to_owned() } else { format!("apifa — {}", targets.join(", ")) });
    let text = crate::docgen::generate(&items, docs(), &crate::docgen::Options { title, examples, data_map: crate::datamap::load() }, format);
    match output {
        Some(file) => {
            if let Err(e) = std::fs::write(&file, text) {
                eprintln!("can't write {file}: {e}");
                return 1;
            }
            eprintln!("{} request(s) / flow(s) documented in {file}", items.len());
        }
        None => out!("{text}"),
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_probe() {
        assert_eq!(replace_placeholders(r#"{"id": {{user.body.id}}, "s": "{{now}}"}"#), r#"{"id": 0, "s": "0"}"#);
    }
}
