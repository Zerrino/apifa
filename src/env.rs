//! Environments (dev, staging, prod…): named sets of values (`baseUrl`, `token`, `user_id`…) used as
//! `{{name}}` in every request (URL, path variables, fields, query, headers, body, auth) and as
//! starting variables of flows. One is active at a time; its `baseUrl` is where relative paths go.
//! Saved in `environments.json`, next to `workspace/`.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::model::Kv;

pub const FILE: &str = "environments.json";

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Environments {
    /// Name of the active one (None: no environment).
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub list: Vec<Environment>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Environment {
    pub name: String,
    #[serde(default)]
    pub vars: Vec<Kv>,
    /// Names whose values are secrets: masked in the editor, never given to an AI.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
}

impl Environments {
    /// The file, or — the first time — `dev` and `prod`, each with an empty `baseUrl` to fill.
    pub fn load() -> Self {
        if !crate::storage::dir().join(FILE).exists() {
            let env = |name: &str| Environment { name: name.into(), vars: vec![Kv { enabled: true, key: "baseUrl".into(), value: String::new() }], secrets: Vec::new() };
            return Environments { active: None, list: vec![env("dev"), env("prod")] };
        }
        crate::storage::load(FILE)
    }

    pub fn save(&self) {
        crate::storage::save(FILE, self);
    }

    pub fn active(&self) -> Option<&Environment> {
        let name = self.active.as_deref()?;
        self.list.iter().find(|e| e.name == name)
    }

    /// The active environment's enabled values, as (name, value).
    pub fn active_vars(&self) -> Vec<(String, String)> {
        self.active()
            .map(|e| e.vars.iter().filter(|kv| kv.enabled && !kv.key.trim().is_empty()).map(|kv| (kv.key.trim().to_owned(), kv.value.clone())).collect())
            .unwrap_or_default()
    }

    /// Makes `name` the active one (`None`: none). An unknown name is an error.
    pub fn select(&mut self, name: Option<&str>) -> Result<(), String> {
        match name {
            None => self.active = None,
            Some(n) if self.list.iter().any(|e| e.name == n) => self.active = Some(n.to_owned()),
            Some(n) => {
                let names: Vec<&str> = self.list.iter().map(|e| e.name.as_str()).collect();
                return Err(format!("No environment named “{n}” (there are: {})", if names.is_empty() { "none".into() } else { names.join(", ") }));
            }
        }
        Ok(())
    }
}

/// The values requests and flows use now (the active environment's), set by the app and the CLI.
static CURRENT: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());

pub fn set_current(vars: Vec<(String, String)>) {
    *CURRENT.write() = vars;
}

pub fn current() -> Vec<(String, String)> {
    CURRENT.read().clone()
}

/// Variables set by scripts (`pm.variables.set`, `pm.collectionVariables.set`) and globals
/// (`pm.globals.set`): kept while the app runs, never written.
static RUNTIME: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());
static GLOBALS: RwLock<Vec<(String, String)>> = RwLock::new(Vec::new());
/// What scripts changed in the environment (name, new value or None = unset), for the app (or the
/// CLI) to save into the active environment.
static CHANGES: parking_lot::Mutex<Vec<(String, Option<String>)>> = parking_lot::Mutex::new(Vec::new());

pub fn runtime() -> Vec<(String, String)> {
    RUNTIME.read().clone()
}

pub fn globals() -> Vec<(String, String)> {
    GLOBALS.read().clone()
}

/// A script's result: the environment as it left it (compared with `before`), its variables and globals.
pub fn apply_script(before: &[(String, String)], after: &[(String, String)], runtime: Vec<(String, String)>, globals: Vec<(String, String)>) {
    let mut changes = Vec::new();
    for (k, v) in after {
        if before.iter().find(|(b, _)| b == k).map(|(_, bv)| bv) != Some(v) {
            changes.push((k.clone(), Some(v.clone())));
        }
    }
    for (k, _) in before {
        if !after.iter().any(|(a, _)| a == k) {
            changes.push((k.clone(), None));
        }
    }
    if !changes.is_empty() {
        let mut cur = CURRENT.write();
        for (k, v) in &changes {
            cur.retain(|(n, _)| n != k);
            if let Some(v) = v {
                cur.push((k.clone(), v.clone()));
            }
        }
        CHANGES.lock().extend(changes);
    }
    *RUNTIME.write() = runtime;
    *GLOBALS.write() = globals;
}

/// The environment changes made by scripts since the last call.
pub fn take_changes() -> Vec<(String, Option<String>)> {
    std::mem::take(&mut *CHANGES.lock())
}

impl Environments {
    /// Applies scripts' changes to the active environment; returns whether anything changed.
    pub fn apply_changes(&mut self, changes: &[(String, Option<String>)]) -> bool {
        let Some(name) = self.active.clone() else { return false };
        let Some(env) = self.list.iter_mut().find(|e| e.name == name) else { return false };
        for (k, v) in changes {
            match (env.vars.iter_mut().find(|kv| kv.key.trim() == k), v) {
                (Some(kv), Some(v)) => {
                    kv.value = v.clone();
                    kv.enabled = true;
                }
                (None, Some(v)) => env.vars.push(Kv { enabled: true, key: k.clone(), value: v.clone() }),
                (Some(_), None) => env.vars.retain(|kv| kv.key.trim() != k),
                (None, None) => {}
            }
        }
        !changes.is_empty()
    }
}

/// `{{name}}` replaced by its value: script variables first, then the current environment, then
/// globals; anything else is left as it is (dates, flow variables, an unknown name).
pub fn expand(s: &str) -> String {
    if !s.contains("{{") {
        return s.to_owned();
    }
    let (runtime, env, globals) = (RUNTIME.read(), CURRENT.read(), GLOBALS.read());
    if runtime.is_empty() && env.is_empty() && globals.is_empty() {
        return s.to_owned();
    }
    let all: Vec<(String, String)> = runtime.iter().chain(env.iter()).chain(globals.iter()).cloned().collect();
    expand_with(s, &all)
}

fn expand_with(s: &str, vars: &[(String, String)]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = after[..end].trim();
        match vars.iter().find(|(k, _)| k == name) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_of_the_active_environment() {
        let kv = |k: &str, v: &str| Kv { enabled: true, key: k.into(), value: v.into() };
        let mut envs = Environments {
            active: None,
            list: vec![
                Environment { name: "dev".into(), vars: vec![kv("store_id", "12"), Kv { enabled: false, ..kv("off", "x") }], secrets: Vec::new() },
                Environment { name: "prod".into(), vars: vec![kv("store_id", "1")], secrets: Vec::new() },
            ],
        };
        assert!(envs.active_vars().is_empty());
        envs.select(Some("dev")).unwrap();
        assert_eq!(envs.active_vars(), vec![("store_id".to_owned(), "12".to_owned())], "disabled rows are left out");
        assert!(envs.select(Some("staging")).unwrap_err().contains("dev, prod"));

        let vars = envs.active_vars();
        assert_eq!(expand_with("/v1/stores/{{store_id}}/users", &vars), "/v1/stores/12/users");
        assert_eq!(expand_with("{{ store_id }},{{now}},{{login}}", &vars), "12,{{now}},{{login}}", "the rest is left for later");
        assert_eq!(expand_with("{{store_id", &vars), "{{store_id");
    }
}
