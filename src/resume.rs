//! Stop a flow and resume it later: every successful response of a run is appended to
//! `runs/<flow uid>.resume.jsonl` (data folder) as it arrives, so a run stopped, failed, killed (Ctrl+C,
//! app closed) or cut by the hourly limit can go on where it was left.
//!
//! Resuming runs the flow again from the top with the same inputs, but a request already answered
//! is taken from the file instead of being sent: the run replays its way to where it stopped
//! (variables rebuilt as they were), then goes on calling the API. A response is found again by
//! where it was asked (block positions and loop items, `3/i17/0`) and what was asked (verb, path,
//! query — dates such as `{{now}}` left out —, body), so parallel loops and lookups replay too.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::flow::{Block, Flow};
use crate::model::Prepared;
use crate::net::ResponseData;

const FORMAT: &str = "apifa-resume";

/// First line of the file.
#[derive(Clone, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    #[serde(default)]
    pub flow: String,
    /// Unix seconds when the run began.
    #[serde(default)]
    pub at: u64,
    /// The flow's blocks when the run began (an edited flow may not replay to the same place).
    #[serde(default)]
    pub blocks_hash: String,
    /// The input values of the run: a resumed run uses them, whatever the inputs say now.
    #[serde(default)]
    pub inputs: BTreeMap<String, String>,
}

/// One response.
#[derive(Serialize, Deserialize)]
struct Entry {
    k: String,
    s: u16,
    #[serde(default)]
    h: Vec<(String, String)>,
    #[serde(default)]
    b: String,
}

/// What the app shows next to Run: a stopped run of this flow can be resumed.
#[derive(Clone)]
pub struct Info {
    pub header: Header,
    pub responses: usize,
    /// Unix seconds of the last response kept.
    pub stopped_at: u64,
    /// The flow's blocks were changed since that run.
    pub changed: bool,
}

pub fn path(uid: u64) -> PathBuf {
    crate::storage::dir().join("runs").join(format!("{uid}.resume.jsonl"))
}

pub fn discard(uid: u64) {
    std::fs::remove_file(path(uid)).ok();
}

pub fn info(flow: &Flow) -> Option<Info> {
    info_at(&path(flow.uid), flow)
}

fn info_at(p: &Path, flow: &Flow) -> Option<Info> {
    let file = std::fs::File::open(p).ok()?;
    let stopped_at = file.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    let mut lines = BufReader::new(file).lines();
    let header: Header = serde_json::from_str(&lines.next()?.ok()?).ok()?;
    if header.format != FORMAT {
        return None;
    }
    let responses = lines.map_while(Result::ok).filter(|l| !l.trim().is_empty()).count();
    let changed = header.blocks_hash != blocks_hash(&flow.blocks);
    Some(Info { header, responses, stopped_at, changed })
}

/// The run's file. `resume`: go on with the stopped run (its inputs replace the flow's);
/// otherwise a new file (a stopped run of this flow is forgotten).
pub fn prepare(flow: &mut Flow, resume: bool) -> Result<Checkpoint, String> {
    let file = path(flow.uid);
    if resume {
        let (cp, header) = Checkpoint::open(file)?;
        for input in &mut flow.inputs {
            if let Some(v) = header.inputs.get(input.name.trim()) {
                input.value = v.clone();
            }
        }
        // `run … -i extra=x` can give inputs the flow doesn't declare.
        for (k, v) in &header.inputs {
            if !flow.inputs.iter().any(|i| i.name.trim() == k) {
                flow.inputs.push(crate::flow::FlowInput { name: k.clone(), value: v.clone(), ..Default::default() });
            }
        }
        return Ok(cp);
    }
    Checkpoint::create(file, &header(flow))
}

pub fn header(flow: &Flow) -> Header {
    Header {
        format: FORMAT.into(),
        flow: flow.name.clone(),
        at: crate::storage::now(),
        blocks_hash: blocks_hash(&flow.blocks),
        inputs: flow.inputs.iter().filter(|i| !i.name.trim().is_empty()).map(|i| (i.name.trim().to_owned(), i.value.clone())).collect(),
    }
}

/// FNV-1a of the blocks' JSON (stable from one build to the next, unlike `DefaultHasher`).
pub fn blocks_hash(blocks: &[Block]) -> String {
    let text = serde_json::to_string(blocks).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

/// The run's file: responses of the previous attempts (resume) and the new ones being added.
pub struct Checkpoint {
    cached: HashMap<String, Entry>,
    /// Every position (and its parents) that got a response, to skip `wait` blocks already passed.
    reached: HashSet<String>,
    out: Mutex<Option<std::fs::File>>,
    /// Responses written by this run.
    added: AtomicUsize,
    pub file: PathBuf,
}

impl Checkpoint {
    /// A fresh run: the file starts over with this header.
    pub fn create(file: PathBuf, header: &Header) -> Result<Self, String> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        }
        let mut f = std::fs::File::create(&file).map_err(|e| format!("can't write {}: {e}", file.display()))?;
        writeln!(f, "{}", serde_json::to_string(header).unwrap_or_default()).map_err(|e| e.to_string())?;
        Ok(Self { cached: HashMap::new(), reached: HashSet::new(), out: Mutex::new(Some(f)), added: AtomicUsize::new(0), file })
    }

    /// The file of a stopped run: its header and responses; new responses are added at the end.
    pub fn open(file: PathBuf) -> Result<(Self, Header), String> {
        let f = std::fs::File::open(&file).map_err(|e| format!("no stopped run to resume ({}: {e})", file.display()))?;
        let mut lines = BufReader::new(f).lines();
        let first = lines.next().and_then(Result::ok).unwrap_or_default();
        let header: Header = serde_json::from_str(&first).map_err(|_| format!("{} is not a resume file", file.display()))?;
        if header.format != FORMAT {
            return Err(format!("{} is not a resume file", file.display()));
        }
        let mut me = Self { cached: HashMap::new(), reached: HashSet::new(), out: Mutex::new(None), added: AtomicUsize::new(0), file: file.clone() };
        // A line cut by a crash is skipped.
        for e in lines.map_while(Result::ok).filter_map(|l| serde_json::from_str::<Entry>(&l).ok()) {
            me.mark_reached(&e.k);
            me.cached.insert(e.k.clone(), e);
        }
        let mut out = std::fs::OpenOptions::new().append(true).open(&file).map_err(|e| format!("can't write {}: {e}", file.display()))?;
        // After a line cut by a crash, new entries start on their own line (blank lines are skipped).
        let _ = std::io::Write::write_all(&mut out, b"\n");
        me.out = Mutex::new(Some(out));
        Ok((me, header))
    }

    /// Responses of the earlier attempts.
    pub fn len(&self) -> usize {
        self.cached.len()
    }

    pub fn added(&self) -> usize {
        self.added.load(Ordering::Relaxed)
    }

    fn mark_reached(&mut self, key: &str) {
        let site = key.split_once('|').map_or(key, |(s, _)| s);
        let mut prefix = String::new();
        for seg in site.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(seg);
            self.reached.insert(prefix.clone());
        }
    }

    /// The response kept for `key` by an earlier attempt.
    pub fn get(&self, key: &str) -> Option<ResponseData> {
        let e = self.cached.get(key)?;
        Some(ResponseData {
            status: e.s,
            status_text: String::new(),
            elapsed: std::time::Duration::ZERO,
            size: e.b.len(),
            headers: e.h.clone(),
            body: e.b.clone(),
            is_json: true,
            notes: Vec::new(),
            script: None,
        })
    }

    /// Keeps a response (written at once: a killed run loses nothing).
    pub fn put(&self, key: &str, resp: &ResponseData) {
        // Errors are asked again when resuming (a 500, a 429, a 404 fixed in the meantime…).
        if resp.status >= 400 {
            return;
        }
        let line = serde_json::to_string(&Entry { k: key.to_owned(), s: resp.status, h: resp.headers.clone(), b: resp.body.clone() }).unwrap_or_default();
        if let Some(f) = self.out.lock().as_mut() {
            if writeln!(f, "{line}").is_ok() {
                self.added.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// A `wait` at `site` was already passed: something after it (a later block, a later loop item,
    /// at its level or above) got a response.
    pub fn passed(&self, site: &[String]) -> bool {
        (0..site.len()).rev().any(|level| {
            let parent = site[..level].join("/");
            let (kind, n) = split_seg(&site[level]);
            (n + 1..n + 65).any(|m| {
                let seg = format!("{kind}{m}");
                self.reached.contains(&if parent.is_empty() { seg } else { format!("{parent}/{seg}") })
            })
        })
    }

    /// The run ended well: nothing to resume.
    pub fn finish(&self) {
        self.out.lock().take();
        std::fs::remove_file(&self.file).ok();
    }
}

/// `i17` → ("i", 17); `3` → ("", 3).
fn split_seg(seg: &str) -> (&str, u64) {
    let digits = seg.trim_start_matches(|c: char| !c.is_ascii_digit());
    (&seg[..seg.len() - digits.len()], digits.parse().unwrap_or(0))
}

/// What identifies a request at its place: verb, path, query and body, without the dates
/// (`range[begin_at]=…,{{now}}` asks the same thing an hour later).
pub fn signature(req: &Prepared) -> String {
    let undate = |v: &str| v.split(',').map(|p| if crate::dates::parse(p.trim()).is_some() { "*" } else { p }).collect::<Vec<_>>().join(",");
    let mut query: Vec<String> = req.query.iter().map(|(k, v)| format!("{k}={}", undate(v))).collect();
    query.sort();
    // `{"begin_at": "{{now}}"}` too: a resumed POST must find its response, not be sent twice.
    fn undate_json(v: &mut serde_json::Value) {
        match v {
            serde_json::Value::String(s) if crate::dates::parse(s.trim()).is_some() => *s = "*".into(),
            serde_json::Value::Array(a) => a.iter_mut().for_each(undate_json),
            serde_json::Value::Object(o) => o.values_mut().for_each(undate_json),
            _ => {}
        }
    }
    let body = match req.body.as_deref() {
        Some(b) => match serde_json::from_str::<serde_json::Value>(b) {
            Ok(mut v) => {
                undate_json(&mut v);
                v.to_string()
            }
            Err(_) => b.to_owned(),
        },
        None => String::new(),
    };
    format!("{} {}?{}#{body}", req.verb, req.path, query.join("&"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_roundtrip() {
        let dir = std::env::temp_dir().join(format!("ft_resume_{}", std::process::id()));
        let file = dir.join("1.resume.jsonl");
        let header = Header { format: FORMAT.into(), flow: "F".into(), at: 1, blocks_hash: "h".into(), inputs: BTreeMap::from([("login".into(), "jdoe".into())]) };
        let cp = Checkpoint::create(file.clone(), &header).unwrap();
        let ok = ResponseData {
            status: 200,
            status_text: String::new(),
            elapsed: Default::default(),
            size: 2,
            headers: vec![("x-total".into(), "3".into())],
            body: "[1]".into(),
            is_json: true,
            notes: Vec::new(),
            script: None,
        };
        cp.put("0|GET /v1/users?#", &ok);
        cp.put("2/i3/0|GET /v1/users/1?#", &ok);
        cp.put("2/i4/0|GET /v1/users/2?#", &ResponseData { status: 500, ..ok });
        drop(cp);
        let (cp, back) = Checkpoint::open(file.clone()).unwrap();
        assert_eq!(back.inputs["login"], "jdoe");
        assert_eq!(cp.len(), 2, "errors aren't kept");
        assert_eq!(cp.get("0|GET /v1/users?#").unwrap().header_u64("x-total"), Some(3));
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // A wait before block 2 was passed; one in item 3 too (item 3 got further); one after isn't.
        assert!(cp.passed(&s(&["1"])));
        assert!(cp.passed(&s(&["2", "i2", "1"])));
        assert!(!cp.passed(&s(&["2", "i3", "1"])));
        assert!(!cp.passed(&s(&["3"])));
        cp.finish();
        assert!(!file.exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn signatures_ignore_dates() {
        let req = |q: &str| Prepared {
            verb: "GET".into(),
            url: String::new(),
            display_url: String::new(),
            path: "/v1/stores/12/visits".into(),
            query: vec![("range[begin_at]".into(), q.into()), ("page[number]".into(), "2".into())],
            headers: Vec::new(),
            body: None,
            files: Vec::new(),
            missing_path: Vec::new(),
            missing_required: Vec::new(),
            body_error: None,
            ..Default::default()
        };
        assert_eq!(signature(&req("2026-10-12T00:00:00Z,2026-10-16T10:00:00Z")), signature(&req("2026-10-12T00:00:00Z,2026-10-16T11:30:00Z")));
        assert_ne!(signature(&req("a")), signature(&req("b")));
    }
}
