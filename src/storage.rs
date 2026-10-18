//! Everything is stored locally, in the user's config directory (no account, no cloud sync).

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::flow::Flow;
use crate::model::RequestSpec;

pub const MAX_HISTORY: usize = 300;

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    /// Where relative paths (`/users`) go when the environment has no `baseUrl`.
    #[serde(default)]
    pub base_url: String,
    /// The workspace's authentication: requests and folders that inherit use it.
    #[serde(default = "no_auth")]
    pub auth: crate::auth::Auth,
    /// Minimum delay between two requests of a flow.
    #[serde(default = "default_delay")]
    pub flow_delay_ms: u64,
    /// Parallel "for each" loops: items in flight at once.
    #[serde(default = "default_parallel")]
    pub parallel: usize,
    /// Parallel loops: requests per second, `0` = the limit the API announces for the application.
    #[serde(default)]
    pub parallel_rate: f64,
    /// Page limit for "all pages" requests in flows.
    #[serde(default = "default_pages")]
    pub max_pages: u32,
    #[serde(default = "default_true")]
    pub animate_background: bool,
    /// The AI assistant (provider, your API keys, model).
    #[serde(default)]
    pub ai: crate::ai::AiConfig,
    /// MCP server for external AI agents (off by default).
    #[serde(default)]
    pub mcp_enabled: bool,
    #[serde(default = "default_mcp_port")]
    pub mcp_port: u16,
    /// Access key the agents send (`Authorization: Bearer …`), made when the server is first turned on.
    #[serde(default)]
    pub mcp_token: String,
    /// Mock server (a local stand-in for the API), off by default.
    #[serde(default)]
    pub mock_enabled: bool,
    #[serde(default = "default_mock_port")]
    pub mock_port: u16,
    /// The app's requests (tabs, flows, tests) go to the mock server instead of the API.
    #[serde(default)]
    pub mock_target: bool,
}

fn default_mock_port() -> u16 {
    crate::mock::DEFAULT_PORT
}

fn default_mcp_port() -> u16 {
    crate::mcp::DEFAULT_PORT
}

fn no_auth() -> crate::auth::Auth {
    crate::auth::Auth::None
}

fn default_true() -> bool {
    true
}

fn default_delay() -> u64 {
    550
}

fn default_parallel() -> usize {
    8
}

fn default_pages() -> u32 {
    30
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            auth: no_auth(),
            flow_delay_ms: default_delay(),
            parallel: default_parallel(),
            parallel_rate: 0.0,
            max_pages: default_pages(),
            animate_background: true,
            ai: Default::default(),
            mcp_enabled: false,
            mcp_port: default_mcp_port(),
            mcp_token: String::new(),
            mock_enabled: false,
            mock_port: default_mock_port(),
            mock_target: false,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub title: String,
    pub verb: String,
    pub url: String,
    pub status: Option<u16>,
    /// Unix timestamp, seconds.
    pub when: u64,
    pub spec: RequestSpec,
}

#[derive(Default, Serialize, Deserialize)]
pub struct SavedTab {
    pub title: String,
    #[serde(default)]
    pub saved: Option<u64>,
    #[serde(default)]
    pub spec: Option<RequestSpec>,
    #[serde(default)]
    pub flow: Option<Flow>,
    /// Whether the tab held edits not saved in its file. `Some(false)`: it reopens from the file,
    /// which may have changed since (an editor, an agent). Missing in older sessions: the copy is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsaved: Option<bool>,
    /// The Data map tab (its map is in `datamap.json`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub data_map: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub tabs: Vec<SavedTab>,
    #[serde(default)]
    pub active: usize,
    /// Share of the editor height given to the response / console.
    #[serde(default = "default_split")]
    pub split: f32,
}

fn default_split() -> f32 {
    0.4
}

impl Default for Session {
    fn default() -> Self {
        Self { tabs: Vec::new(), active: 0, split: default_split() }
    }
}

/// The app's data folder. `APIFA_DIR` overrides it (tests, trying changes on a copy):
/// `XDG_CONFIG_HOME` alone isn't enough, macOS ignores it.
pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("APIFA_DIR") {
        return PathBuf::from(d);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("apifa")
}

/// Tests that point the data folder (`APIFA_DIR`, one per process) somewhere: one at a time.
/// Returns the lock and a fresh, empty folder named after the test.
#[cfg(test)]
pub(crate) fn test_data_dir(name: &str) -> (parking_lot::MutexGuard<'static, ()>, PathBuf) {
    static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let guard = LOCK.lock();
    let dir = std::env::temp_dir().join(format!("ft_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    std::env::set_var("APIFA_DIR", &dir);
    (guard, dir)
}

/// `Ok(None)` when the file doesn't exist. An unreadable file is moved aside (never overwritten).
pub fn load_checked<T: DeserializeOwned>(name: &str) -> Result<Option<T>, String> {
    let path = dir().join(name);
    let Ok(text) = fs::read_to_string(&path) else { return Ok(None) };
    serde_json::from_str(&text).map(Some).map_err(|e| {
        let backup = dir().join(format!("{name}.broken-{}", now()));
        let _ = fs::rename(&path, &backup);
        format!("{name} could not be read ({e}); it was kept as {}", backup.display())
    })
}

/// Files `load` couldn't read since the last call (the app shows them).
static PROBLEMS: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());

/// A problem with the data files, shown by the app (and printed for the command line).
pub fn report(msg: String) {
    eprintln!("{msg}");
    PROBLEMS.lock().push(msg);
}

pub fn take_problems() -> Vec<String> {
    std::mem::take(&mut *PROBLEMS.lock())
}

/// The file's content, or the default. An unreadable file is copied aside first: the next save
/// replaces it (a typo in a hand-edited substitutions.json mustn't cost every rule).
pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    let path = dir().join(name);
    let Ok(text) = fs::read_to_string(&path) else { return T::default() };
    match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            let backup = dir().join(format!("{name}.broken"));
            let _ = fs::copy(&path, &backup);
            report(format!("{name} could not be read ({e}); a copy was kept as {}", backup.display()));
            T::default()
        }
    }
}

/// Atomic write, readable by the current user only (the config holds the app SECRET).
pub fn save<T: Serialize>(name: &str, value: &T) {
    let Ok(json) = serde_json::to_string_pretty(value) else { return };
    write(name, &json);
}

/// Like `save`, but only when the content differs from the last one written this way (`last`: its hash).
pub fn save_if_changed<T: Serialize>(name: &str, value: &T, last: &mut u64) {
    use std::hash::{Hash, Hasher};
    let Ok(json) = serde_json::to_string_pretty(value) else { return };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    json.hash(&mut h);
    let hash = h.finish();
    if hash != *last {
        write(name, &json);
        *last = hash;
    }
}

fn write(name: &str, json: &str) {
    let dir = dir();
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    let tmp = dir.join(format!(".{name}.tmp"));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = opts.open(&tmp).and_then(|mut f| f.write_all(json.as_bytes()));
    if written.is_ok() {
        let _ = fs::rename(&tmp, dir.join(name));
    }
}

/// Atomic write of any file (temporary file, then rename).
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn ago(ts: u64) -> String {
    let d = now().saturating_sub(ts);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => format!("{} d ago", d / 86_400),
    }
}
