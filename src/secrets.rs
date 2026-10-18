//! The secrets of the config (the workspace authentication's password / token / client secret, the
//! AI providers' keys, the MCP access key) live in the system's keychain (macOS Keychain, Secret Service on Linux, Windows Credential
//! Manager) instead of `config.json`: a backup, a synced dotfiles folder or a copy of the data folder
//! then holds no secret.
//!
//! One keychain entry holds them all (one access, at most one prompt), named after the data folder:
//! a copy of the data (`APIFA_DIR`) has its own. Without a keychain (a Linux session without
//! Secret Service…), or with `APIFA_KEYCHAIN=0`, they stay in `config.json` as before.
//! Secrets found in `config.json` (older versions) move to the keychain on the next start.

use std::collections::BTreeMap;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::storage::{self, Config};

const SERVICE: &str = "apifa";
const FILE: &str = "config.json";

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
struct Secrets {
    /// The workspace auth (its secrets; the rest is in the file too).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth: Option<crate::auth::Auth>,
    #[serde(default)]
    ai_keys: BTreeMap<String, String>,
    #[serde(default)]
    mcp_token: String,
}

impl Secrets {
    fn of(c: &Config) -> Self {
        Secrets { auth: c.auth.has_secrets().then(|| c.auth.clone()), ai_keys: c.ai.keys.clone(), mcp_token: c.mcp_token.clone() }
    }

    fn is_empty(&self) -> bool {
        self.auth.is_none() && self.ai_keys.values().all(String::is_empty) && self.mcp_token.is_empty()
    }
}

/// What the keychain holds (as last read or written): saving the config only writes it when it changed.
static IN_KEYCHAIN: Mutex<Option<Secrets>> = Mutex::new(None);

fn enabled() -> bool {
    !cfg!(test) && std::env::var("APIFA_KEYCHAIN").map_or(true, |v| v != "0")
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, &format!("secrets {}", storage::dir().display())).map_err(|e| e.to_string())
}

fn read() -> Result<Option<Secrets>, String> {
    match entry()?.get_password() {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|e| format!("the keychain entry is not readable: {e}")),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn write(s: &Secrets) -> Result<(), String> {
    let e = entry()?;
    if s.is_empty() {
        return match e.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    e.set_password(&serde_json::to_string(s).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// The config without its secrets, as written to `config.json`.
fn without_secrets(c: &Config) -> Config {
    let mut c = c.clone();
    c.auth = c.auth.without_secrets();
    c.ai.keys.values_mut().for_each(String::clear);
    c.ai.keys.retain(|_, v| !v.is_empty());
    c.mcp_token.clear();
    c
}

fn fill(c: &mut Config, s: &Secrets) {
    if let Some(a) = &s.auth {
        c.auth.fill_secrets(a);
    }
    for (k, v) in &s.ai_keys {
        let key = c.ai.keys.entry(k.clone()).or_default();
        if key.is_empty() {
            *key = v.clone();
        }
    }
    if c.mcp_token.is_empty() {
        c.mcp_token = s.mcp_token.clone();
    }
}

/// The config with its secrets. A problem with the keychain is reported once (`storage::take_problems`).
pub fn load_config() -> Config {
    load_raw()
}

fn load_raw() -> Config {
    let mut c: Config = storage::load(FILE);
    if !enabled() {
        return c;
    }
    match read() {
        Ok(stored) => {
            let in_file = Secrets::of(&c);
            if let Some(s) = &stored {
                fill(&mut c, s);
            }
            *IN_KEYCHAIN.lock() = stored;
            // Secrets still in the file (an older version wrote them there): into the keychain.
            if !in_file.is_empty() {
                if let Err(e) = save_config(&c) {
                    storage::report(format!("The secrets stay in {FILE}: the keychain refused them ({e})"));
                }
            }
        }
        Err(e) => storage::report(format!("The keychain can't be read ({e}): the secrets stay in {FILE}")),
    }
    c
}

/// Writes the config: the secrets to the keychain (when they changed), the rest to `config.json`.
/// If the keychain refuses them, everything goes to the file (nothing is lost) and the error is returned.
pub fn save_config(c: &Config) -> Result<(), String> {
    if !enabled() {
        storage::save(FILE, c);
        return Ok(());
    }
    let s = Secrets::of(c);
    let mut known = IN_KEYCHAIN.lock();
    if known.as_ref() != Some(&s) {
        if let Err(e) = write(&s) {
            storage::save(FILE, c);
            return Err(e);
        }
        *known = Some(s);
    }
    storage::save(FILE, &without_secrets(c));
    Ok(())
}

/// Removes the keychain entry (uninstall with the data).
pub fn forget() {
    if enabled() {
        if let Ok(e) = entry() {
            let _ = e.delete_credential();
        }
        *IN_KEYCHAIN.lock() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_keeps_no_secret() {
        let auth = crate::auth::Auth::Basic { username: "me".into(), password: "s3cret".into() };
        let mut c = Config { auth: auth.clone(), mcp_token: "tok".into(), ..Default::default() };
        c.ai.keys.insert("anthropic".into(), "sk-ant".into());
        let file = without_secrets(&c);
        assert_eq!(file.auth, crate::auth::Auth::Basic { username: "me".into(), password: String::new() }, "the user name isn't a secret");
        assert!(file.mcp_token.is_empty() && file.ai.keys.is_empty());
        let text = serde_json::to_string(&file).unwrap();
        assert!(!text.contains("s3cret") && !text.contains("sk-ant") && !text.contains("tok\""), "{text}");

        // Back from the keychain: what the file has wins (it's newer).
        let mut back = file.clone();
        back.mcp_token = "newer".into();
        fill(&mut back, &Secrets::of(&c));
        assert_eq!(back.mcp_token, "newer");
        assert_eq!(back.ai.keys["anthropic"], "sk-ant");
        assert_eq!(back.auth, auth);
    }
}
