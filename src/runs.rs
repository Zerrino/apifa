//! A flow run's results kept in a file ("Keep results", Export / Load): reuse them — the visualization,
//! the outputs, the variables — without running the flow (and calling the API) again.
//! Kept in the data folder (`runs/<flow uid>.json`), not in the workspace: they can be big and personal.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedRun {
    #[serde(default)]
    pub flow: String,
    #[serde(default)]
    pub ok: bool,
    /// Unix seconds.
    #[serde(default)]
    pub at: u64,
    #[serde(default)]
    pub duration_ms: u64,
    /// Every variable at the end of the run (outputs are computed from them).
    pub vars: Value,
}

fn path(base: &std::path::Path, uid: u64) -> PathBuf {
    base.join("runs").join(format!("{uid}.json"))
}

pub fn save(uid: u64, run: &SavedRun) -> Result<(), String> {
    save_in(&crate::storage::dir(), uid, run)
}

pub fn load(uid: u64) -> Option<SavedRun> {
    load_in(&crate::storage::dir(), uid)
}

fn save_in(base: &std::path::Path, uid: u64, run: &SavedRun) -> Result<(), String> {
    let p = path(base, uid);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string(run).map_err(|e| e.to_string())?;
    std::fs::write(&p, text).map_err(|e| format!("can't write {}: {e}", p.display()))
}

fn load_in(base: &std::path::Path, uid: u64) -> Option<SavedRun> {
    parse(&std::fs::read_to_string(path(base, uid)).ok()?).ok()
}

/// A saved run, or plain variables (`apifa run … --vars` prints them).
pub fn parse(text: &str) -> Result<SavedRun, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("not JSON: {e}"))?;
    // A saved run has all its fields: `--vars` output can have a variable named `vars`.
    if v.get("vars").is_some_and(Value::is_object) && v.get("flow").is_some() && v.get("at").is_some() {
        return serde_json::from_value(v).map_err(|e| e.to_string());
    }
    if v.is_object() {
        return Ok(SavedRun { flow: String::new(), ok: true, at: 0, duration_ms: 0, vars: v });
    }
    Err("expected the results of a run (a JSON object of variables)".into())
}

/// `2026-10-16 14:02` (local time), or "" when unknown.
pub fn when(at: u64) -> String {
    use chrono::TimeZone;
    if at == 0 {
        return String::new();
    }
    chrono::Local.timestamp_opt(at as i64, 0).single().map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_runs() {
        let dir = std::env::temp_dir().join(format!("ft_runs_{}", std::process::id()));
        let run = SavedRun { flow: "F".into(), ok: true, at: 1_760_000_000, duration_ms: 1500, vars: serde_json::json!({ "users": [1, 2] }) };
        save_in(&dir, 17, &run).unwrap();
        let back = load_in(&dir, 17).unwrap();
        assert_eq!(back.vars, run.vars);
        assert_eq!(back.duration_ms, 1500);
        assert!(load_in(&dir, 43).is_none());
        // Plain variables (from `run --vars`) load too.
        assert_eq!(parse(r#"{"loc": {"status": 200}}"#).unwrap().vars["loc"]["status"], 200);
        assert!(parse("[1,2]").is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
