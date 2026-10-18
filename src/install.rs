//! Detects how the app was installed (install.sh / `nix-env` / macOS `.app`) and removes it (Settings › Uninstall).

use std::path::PathBuf;
use std::process::Command;

const APP: &str = "apifa";
#[cfg(not(target_os = "macos"))]
const ICON_SIZES: [u32; 4] = [48, 128, 256, 512];

/// The `.app` bundle made by install.sh on macOS.
#[cfg(target_os = "macos")]
fn mac_app() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Applications/apifa.app"))
}

#[cfg(target_os = "macos")]
fn local_files() -> Vec<PathBuf> {
    let bin = std::env::var_os("XDG_BIN_HOME").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".local/bin")));
    // The command is a symlink into the bundle: only ours is removed.
    let link = bin.map(|b| b.join(APP)).filter(|l| std::fs::read_link(l).is_ok_and(|t| mac_app().is_some_and(|app| t.starts_with(app))));
    mac_app().into_iter().chain(link).collect()
}

#[cfg(not(target_os = "macos"))]
fn local_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(bin) = dirs::executable_dir() {
        files.push(bin.join(APP));
    }
    if let Some(data) = dirs::data_local_dir() {
        files.push(data.join("applications").join(format!("{APP}.desktop")));
        for s in ICON_SIZES {
            files.push(data.join(format!("icons/hicolor/{s}x{s}/apps/{APP}.png")));
        }
    }
    files
}

#[derive(Clone, Copy)]
enum NixProfile {
    /// `nix profile` (new-style profiles)
    Profile,
    /// `nix-env` (legacy profiles)
    Env,
}

fn run(cmd: &str, args: &[&str]) -> Option<std::process::Output> {
    Command::new(cmd).args(args).output().ok()
}

fn nix_install() -> Option<NixProfile> {
    if let Some(out) = run("nix", &["profile", "list"]) {
        if out.status.success() && String::from_utf8_lossy(&out.stdout).contains(APP) {
            return Some(NixProfile::Profile);
        }
    }
    run("nix-env", &["-q", APP]).filter(|o| o.status.success() && !o.stdout.is_empty()).map(|_| NixProfile::Env)
}

pub struct Status {
    nix: Option<NixProfile>,
    pub local: bool,
}

impl Status {
    pub fn installed(&self) -> bool {
        self.nix.is_some() || self.local
    }

    pub fn describe(&self) -> &'static str {
        match (self.nix.is_some(), self.local) {
            (true, _) => "Installed as an application (Nix user profile).",
            (false, true) if cfg!(target_os = "macos") => "Installed as an application (~/Applications).",
            (false, true) => "Installed as an application (~/.local).",
            _ => "Not installed as an application (running from the project folder). Run ./install.sh to add it to your apps.",
        }
    }
}

/// Also true for a dangling symlink.
fn present(f: &std::path::Path) -> bool {
    f.symlink_metadata().is_ok()
}

pub fn status() -> Status {
    Status { nix: nix_install(), local: local_files().iter().any(|f| present(f)) }
}

/// Removes the installed app; with `delete_data`, also the credentials, workspace and history.
pub fn uninstall(delete_data: bool) -> Result<String, String> {
    let st = status();
    let mut done = Vec::new();
    if let Some(kind) = st.nix {
        let (cmd, args): (&str, &[&str]) = match kind {
            NixProfile::Profile => ("nix", &["profile", "remove", APP]),
            NixProfile::Env => ("nix-env", &["-e", APP]),
        };
        let out = Command::new(cmd).args(args).output().map_err(|e| format!("{cmd}: {e}"))?;
        if !out.status.success() {
            return Err(format!("{cmd} {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
        }
        done.push("removed from your Nix profile");
    }
    let mut removed = false;
    for f in local_files().into_iter().filter(|f| present(f)) {
        let res = if f.symlink_metadata().is_ok_and(|m| m.is_dir()) { std::fs::remove_dir_all(&f) } else { std::fs::remove_file(&f) };
        res.map_err(|e| format!("Can't remove {}: {e}", f.display()))?;
        removed = true;
    }
    if removed {
        done.push("app files removed");
    }
    if delete_data {
        crate::secrets::forget();
        let dir = crate::storage::dir();
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("Can't delete {}: {e}", dir.display()))?;
        }
        done.push("your data was deleted");
    }
    if done.is_empty() {
        return Err("Nothing to uninstall: the app isn't installed.".into());
    }
    Ok(format!("Uninstalled: {}.", done.join(", ")))
}

#[cfg(test)]
mod tests {
    /// Read-only: prints how the app is installed on this machine. `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn detect_install() {
        let st = super::status();
        println!("{}", st.describe());
    }
}
