//! The workspace on disk, as plain files an editor (or an AI agent) can change directly:
//!
//! ```text
//! ~/.config/apifa/workspace/
//!   .folder.json                         {"uid": …}  (root)
//!   Examples/
//!     .folder.json                       {"uid": …, "name": "Examples"}
//!     List users.request.json            a saved request
//!     Finished projects.flow.json        a flow
//! ```
//!
//! Files are rewritten only when their content changes, files the app can't read are never
//! deleted, and the app reloads when something changes on disk.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::flow::Flow;
use crate::workspace::{new_uid, Folder, Node, SavedRequest};

pub const REQUEST_EXT: &str = ".request.json";
pub const FLOW_EXT: &str = ".flow.json";
const FOLDER_META: &str = ".folder.json";

pub fn dir() -> PathBuf {
    crate::storage::dir().join("workspace")
}

/// Where each node lives on disk (by uid), to delete files of removed / renamed nodes.
#[derive(Default)]
pub struct Index {
    paths: HashMap<u64, PathBuf>,
    folders: HashMap<u64, PathBuf>,
    /// What the app last read or wrote in each file (content hash): a save skips the files that
    /// would get the same content, without reading the whole workspace back.
    contents: HashMap<PathBuf, u64>,
}

fn content_hash(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

pub struct Loaded {
    pub root: Folder,
    /// Folder uids met so far: a copied folder repeats its original's.
    seen: HashSet<u64>,
    /// Where each request / flow uid was read, in reading order, and whether the file is named after it:
    /// a copied file (Finder, `cp`) repeats its original's uid.
    files: HashMap<u64, Vec<(PathBuf, bool)>>,
    pub index: Index,
    /// Files that could not be read (kept untouched on disk).
    pub problems: Vec<String>,
    /// Some files got an id assigned and must be written back.
    pub needs_save: bool,
}

/// `Ok(None)` when there is no workspace folder yet.
pub fn load() -> Option<Loaded> {
    let root_dir = dir();
    if !root_dir.is_dir() {
        return None;
    }
    let mut ctx =
        Loaded { root: Folder::new("Workspace"), seen: HashSet::new(), files: HashMap::new(), index: Index::default(), problems: Vec::new(), needs_save: false };
    let mut root = read_folder(&root_dir, "Workspace", &mut ctx);
    root.name = "Workspace".into();
    let files = std::mem::take(&mut ctx.files);
    let mut renew: HashMap<u64, (usize, Vec<PathBuf>)> = HashMap::new();
    for (uid, at) in files {
        if at.len() == 1 {
            ctx.index.paths.insert(uid, at.into_iter().next().expect("one").0);
            continue;
        }
        // The original keeps the uid (tabs, favorites, kept results and flow blocks point to it): the file
        // named after it ("Hand.flow.json", not "Hand copy.flow.json"), else the first one read.
        let keep = at.iter().position(|(_, named)| *named).unwrap_or(0);
        ctx.index.paths.insert(uid, at[keep].0.clone());
        renew.insert(uid, (keep, at.into_iter().map(|(p, _)| p).collect()));
        ctx.needs_save = true;
    }
    if !renew.is_empty() {
        // The tree is in reading order: the n-th node with a uid is its n-th file.
        fn walk(f: &mut Folder, renew: &HashMap<u64, (usize, Vec<PathBuf>)>, count: &mut HashMap<u64, usize>, index: &mut Index) {
            for node in &mut f.items {
                match node {
                    Node::Folder(sub) => walk(sub, renew, count, index),
                    _ => {
                        let uid = node.uid();
                        let Some((keep, paths)) = renew.get(&uid) else { continue };
                        let n = count.entry(uid).or_default();
                        if *n != *keep {
                            node.refresh_uids();
                            index.paths.insert(node.uid(), paths[*n].clone());
                        }
                        *n += 1;
                    }
                }
            }
        }
        walk(&mut root, &renew, &mut HashMap::new(), &mut ctx.index);
    }
    ctx.root = root;
    Some(ctx)
}

/// The folder's uid, saved name and auth (`.folder.json`).
fn read_meta(dir: &Path, ctx: &mut Loaded) -> (u64, Option<String>, crate::auth::Auth, String) {
    let text = fs::read_to_string(dir.join(FOLDER_META)).ok();
    if let Some(t) = &text {
        ctx.index.contents.insert(dir.join(FOLDER_META), content_hash(t));
    }
    let meta = text.and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let name = meta.as_ref().and_then(|v| v.get("name")).and_then(Value::as_str).map(str::to_owned);
    let auth = meta.as_ref().and_then(|v| v.get("auth")).and_then(|a| serde_json::from_value(a.clone()).ok()).unwrap_or_default();
    let profile = meta.as_ref().and_then(|v| v.get("profile")).and_then(Value::as_str).unwrap_or_default().to_owned();
    match meta.as_ref().and_then(|v| v.get("uid")).and_then(Value::as_u64) {
        Some(uid) if ctx.seen.insert(uid) => (uid, name, auth, profile),
        _ => {
            ctx.needs_save = true;
            let uid = new_uid();
            ctx.seen.insert(uid);
            (uid, name, auth, profile)
        }
    }
}

fn stem<'a>(file: &'a str, ext: &str) -> &'a str {
    file.strip_suffix(ext).unwrap_or(file)
}

fn read_folder(path: &Path, name: &str, ctx: &mut Loaded) -> Folder {
    let (uid, saved, auth, profile) = read_meta(path, ctx);
    // The saved name when it still fits the folder (it can hold "/", differ by case on macOS, or only by the
    // " (2)" given to a second folder of the same name): else the folder was renamed outside, its name wins.
    let fits = |s: &str| {
        let (dir, base) = (name.to_lowercase(), file_name(s).to_lowercase());
        dir == base
            || dir
                .strip_prefix(base.as_str())
                .and_then(|r| r.strip_prefix(" ("))
                .and_then(|r| r.strip_suffix(')'))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    let name = match saved {
        Some(s) if fits(&s) => s,
        _ => name.to_owned(),
    };
    let mut folder = Folder { uid, name, items: Vec::new(), auth, profile };
    ctx.index.folders.insert(folder.uid, path.to_owned());
    let mut entries: Vec<_> = fs::read_dir(path).map(|rd| rd.flatten().collect()).unwrap_or_default();
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    for e in entries {
        let p = e.path();
        let file = e.file_name().to_string_lossy().into_owned();
        if file.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            folder.items.push(Node::Folder(read_folder(&p, &file, ctx)));
            continue;
        }
        let (is_flow, ext) = if file.ends_with(FLOW_EXT) {
            (true, FLOW_EXT)
        } else if file.ends_with(REQUEST_EXT) {
            (false, REQUEST_EXT)
        } else {
            continue;
        };
        match read_node(&p, is_flow, stem(&file, ext)) {
            Ok((node, had_uid, hash)) => {
                ctx.index.contents.insert(p.clone(), hash);
                if !had_uid {
                    ctx.needs_save = true;
                }
                let named = file_name(node.name()).eq_ignore_ascii_case(stem(&file, ext));
                ctx.files.entry(node.uid()).or_default().push((p, named));
                folder.items.push(node);
            }
            Err(e) => ctx.problems.push(format!("{}: {e}", p.display())),
        }
    }
    folder
}

/// The node, whether its file had a uid, and the hash of the file's content.
fn read_node(path: &Path, is_flow: bool, file_stem: &str) -> Result<(Node, bool, u64), String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let hash = content_hash(&text);
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))?;
    let had_uid = v.get("uid").is_some_and(Value::is_u64);
    let mut node = if is_flow {
        Node::Flow(serde_json::from_value::<Flow>(v).map_err(|e| format!("invalid flow: {e}"))?)
    } else {
        Node::Request(serde_json::from_value::<SavedRequest>(v).map_err(|e| format!("invalid request: {e}"))?)
    };
    if node.name().trim().is_empty() {
        node.set_name(file_stem.to_owned());
    }
    Ok((node, had_uid, hash))
}

/// File-name-safe version of a name.
pub fn file_name(name: &str) -> String {
    let s: String = name.trim().chars().map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let s = s.trim_start_matches('.').trim().to_owned();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

fn unique(base: &str, ext: &str, used: &mut HashSet<String>) -> String {
    let mut candidate = format!("{base}{ext}");
    let mut n = 2;
    while !used.insert(candidate.to_lowercase()) {
        candidate = format!("{base} ({n}){ext}");
        n += 1;
    }
    candidate
}

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)
}

/// Writes the workspace; removes the files of nodes that were deleted, renamed or moved.
pub fn save(root: &Folder, index: &mut Index) -> Result<(), String> {
    let mut new = Index::default();
    write_folder(root, &dir(), None, index, &mut new).map_err(|e| format!("Saving the workspace failed: {e}"))?;
    // An old path can be one just written for another node (B renamed into A's freed name), or the same
    // file under another case (a case-only rename on macOS): those must stay.
    let written: HashSet<FileKey> = new.paths.values().chain(new.folders.values()).filter_map(|p| file_key(p)).collect();
    let kept = |old: &PathBuf| file_key(old).is_some_and(|k| written.contains(&k));
    for (uid, old) in &index.paths {
        match new.paths.get(uid) {
            Some(p) if p == old => {}
            Some(p) if kept(old) => {
                // Same file, other spelling: give it the new one.
                if file_key(p) == file_key(old) {
                    let _ = fs::rename(old, p);
                }
            }
            _ if kept(old) => {}
            _ => {
                let _ = fs::remove_file(old);
            }
        }
    }
    // Deepest first, and only when empty: unknown files keep their folder alive.
    let mut gone: Vec<&PathBuf> = index.folders.iter().filter(|(uid, p)| new.folders.get(uid) != Some(*p) && !kept(p)).map(|(_, p)| p).collect();
    gone.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for p in gone {
        let _ = fs::remove_file(p.join(FOLDER_META));
        let _ = fs::remove_dir(p);
    }
    *index = new;
    Ok(())
}

/// Identifies a file whatever the spelling of its path (inode on Unix, else the lowercased path).
#[cfg(unix)]
type FileKey = (u64, u64);
#[cfg(not(unix))]
type FileKey = String;

#[cfg(unix)]
fn file_key(p: &Path) -> Option<FileKey> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(p).ok().map(|m| (m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_key(p: &Path) -> Option<FileKey> {
    p.exists().then(|| p.to_string_lossy().to_lowercase())
}

/// Writes `content` unless the app already knows the file holds it (`prev`), and records it in `new`.
fn write_known(path: &Path, content: &str, prev: &Index, new: &mut Index) -> std::io::Result<()> {
    let hash = content_hash(content);
    if prev.contents.get(path) != Some(&hash) || !path.exists() {
        write_if_changed(path, content)?;
    }
    new.contents.insert(path.to_owned(), hash);
    Ok(())
}

fn write_folder(folder: &Folder, path: &Path, name: Option<&str>, prev: &Index, index: &mut Index) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    let meta = match name {
        Some(n) => serde_json::json!({ "uid": folder.uid, "name": n }),
        None => serde_json::json!({ "uid": folder.uid }),
    };
    let mut meta = meta;
    if !folder.auth.is_inherit() {
        meta["auth"] = serde_json::to_value(&folder.auth).unwrap_or_default();
    }
    if !folder.profile.is_empty() {
        meta["profile"] = Value::String(folder.profile.clone());
    }
    write_known(&path.join(FOLDER_META), &serde_json::to_string_pretty(&meta).unwrap_or_default(), prev, index)?;
    index.folders.insert(folder.uid, path.to_owned());
    let mut used = HashSet::new();
    for node in &folder.items {
        match node {
            Node::Folder(f) => {
                let dir_name = unique(&file_name(&f.name), "", &mut used);
                write_folder(f, &path.join(&dir_name), Some(&f.name), prev, index)?;
            }
            Node::Request(r) => {
                let p = path.join(unique(&file_name(&r.name), REQUEST_EXT, &mut used));
                write_known(&p, &serde_json::to_string_pretty(r).unwrap_or_default(), prev, index)?;
                index.paths.insert(r.uid, p);
            }
            Node::Flow(fl) => {
                let p = path.join(unique(&file_name(&fl.name), FLOW_EXT, &mut used));
                write_known(&p, &serde_json::to_string_pretty(fl).unwrap_or_default(), prev, index)?;
                index.paths.insert(fl.uid, p);
            }
        }
    }
    Ok(())
}

/// Watches the folder on its own thread: sends `(signature, when it was taken)` each time the files
/// change, then calls `wake`. The system's file events (FSEvents, inotify…) say when to look; without
/// them (a folder that doesn't exist yet, a file system that sends none) it looks every 1.5 s.
/// A look is a `signature`: events from the app's own saves give the signature it already knows.
pub fn watch(wake: impl Fn() + Send + 'static) -> std::sync::mpsc::Receiver<(u64, std::time::Instant)> {
    use notify::Watcher;
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (events_tx, events) = std::sync::mpsc::channel::<()>();
        let watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
            let _ = events_tx.send(());
        })
        .and_then(|mut w| w.watch(&dir(), notify::RecursiveMode::Recursive).map(|()| w));
        // With events, a look now and then anyway (a missed event, a folder created later).
        let every = if watcher.is_ok() { Duration::from_secs(30) } else { Duration::from_millis(1500) };
        let mut last = signature();
        loop {
            match events.recv_timeout(every) {
                Ok(()) => {
                    // A burst (git checkout, an agent writing several files) is one change.
                    std::thread::sleep(Duration::from_millis(150));
                    while events.try_recv().is_ok() {}
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => std::thread::sleep(every),
            }
            let at = Instant::now();
            let sig = signature();
            if sig != last {
                last = sig;
                if tx.send((sig, at)).is_err() {
                    return;
                }
                wake();
            }
        }
    });
    rx
}

/// Cheap fingerprint of the folder (paths, sizes, modification times) to notice outside edits.
pub fn signature() -> u64 {
    fn walk(p: &Path, h: &mut std::collections::hash_map::DefaultHasher) {
        let mut entries: Vec<_> = fs::read_dir(p).map(|rd| rd.flatten().collect()).unwrap_or_default();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let path = e.path();
            // Like loading: hidden folders (.git…) aren't part of the workspace.
            if e.file_name().to_string_lossy().starts_with('.') && e.file_name() != FOLDER_META {
                continue;
            }
            if path.is_dir() {
                walk(&path, h);
            } else if path.extension().is_some_and(|x| x == "json") {
                path.hash(h);
                if let Ok(m) = e.metadata() {
                    m.len().hash(h);
                    m.modified().ok().hash(h);
                }
            }
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    walk(&dir(), &mut h);
    h.finish()
}

/// Agent / editor documentation dropped next to the files.
pub fn write_docs() {
    let d = dir();
    let _ = write_if_changed(&d.join("AGENTS.md"), include_str!("../AGENTS.md"));
    let _ = write_if_changed(&d.join("CLAUDE.md"), "@AGENTS.md\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Save → load → rename → save: same tree, old file removed, unknown files kept.
    #[test]
    fn roundtrip_on_disk() {
        let (_lock, tmp) = crate::storage::test_data_dir("ws_test");
        let mut root = crate::workspace::default_workspace();
        let mut index = Index::default();
        save(&root, &mut index).unwrap();
        let foreign = dir().join("Examples").join("notes.txt");
        fs::write(&foreign, "keep me").unwrap();
        let loaded = load().unwrap();
        assert!(loaded.problems.is_empty() && !loaded.needs_save);
        assert_eq!(loaded.root.count(), root.count());
        let flow_uid = match &root.items[0] {
            Node::Folder(f) => f.items.iter().find(|n| matches!(n, Node::Flow(_))).unwrap().uid(),
            _ => unreachable!(),
        };
        let old = loaded.index.paths[&flow_uid].clone();
        root.find_mut(flow_uid).unwrap().set_name("Renamed".into());
        let mut index = loaded.index;
        save(&root, &mut index).unwrap();
        assert!(!old.exists(), "old file removed");
        assert!(dir().join("Examples/Renamed.flow.json").exists());
        assert!(foreign.exists(), "files the app doesn't own are kept");
        // A file written by hand, without uid, is accepted and gets one.
        fs::write(dir().join("Hand.flow.json"), r#"{"blocks":[{"type":"log","message":"hi"}]}"#).unwrap();
        let again = load().unwrap();
        assert!(again.needs_save);
        assert!(again.root.items.iter().any(|n| n.name() == "Hand"));

        // Two "New request": deleting the first moves the second into its file name, which must not be removed then.
        let mut root = again.root;
        let mut index = again.index;
        let req = |name: &str| Node::Request(crate::workspace::SavedRequest { uid: new_uid(), name: name.into(), spec: Default::default() });
        let (a, b) = (req("New request"), req("New request"));
        let b_uid = b.uid();
        root.items.push(a.clone());
        root.items.push(b);
        save(&root, &mut index).unwrap();
        assert!(dir().join("New request (2).request.json").exists());
        root.items.retain(|n| n.uid() != a.uid());
        save(&root, &mut index).unwrap();
        assert!(dir().join("New request.request.json").exists(), "the second request kept its file");
        assert!(!dir().join("New request (2).request.json").exists());
        // A rename by case only keeps the file (one file on a case-insensitive disk).
        root.find_mut(b_uid).unwrap().set_name("NEW request".into());
        save(&root, &mut index).unwrap();
        let reloaded = load().unwrap();
        assert!(reloaded.root.items.iter().any(|n| n.uid() == b_uid && n.name() == "NEW request"), "renamed by case, still there");
        // A copied file (same uid) gets its own uid.
        fs::copy(dir().join("Hand.flow.json"), dir().join("Hand copy.flow.json")).unwrap();
        let copied = load().unwrap();
        let hand_uid = root.items.iter().find(|n| n.name() == "Hand").map(Node::uid).unwrap();
        let uids: Vec<u64> = copied.root.items.iter().filter(|n| n.name() == "Hand").map(Node::uid).collect();
        assert_eq!(uids.len(), 2);
        assert_ne!(uids[0], uids[1]);
        assert!(copied.needs_save);
        // The original keeps its uid ("hand copy" is read first: ' ' sorts before '.'), the copy gets a new one.
        assert_eq!(copied.index.paths[&hand_uid], dir().join("Hand.flow.json"));
        let copy_uid = *uids.iter().find(|&&u| u != hand_uid).unwrap();
        assert_eq!(copied.index.paths[&copy_uid], dir().join("Hand copy.flow.json"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn names() {
        assert_eq!(file_name("a/b: c?"), "a_b_ c_");
        assert_eq!(file_name("  .hidden "), "hidden");
        let mut used = HashSet::new();
        assert_eq!(unique("x", ".flow.json", &mut used), "x.flow.json");
        assert_eq!(unique("X", ".flow.json", &mut used), "X (2).flow.json");
    }

    // ------------------------------------------------------------ properties

    use proptest::prelude::*;

    /// Names that collide on disk: same file name, case only, characters a file name can't hold.
    const NAMES: [&str; 8] = ["a", "A", "New request", "New request", "x/y", "x_y", " b ", "Users"];

    #[derive(Clone, Debug)]
    enum Op {
        AddRequest {
            folder: usize,
            name: usize,
        },
        AddFlow {
            folder: usize,
            name: usize,
        },
        AddFolder {
            folder: usize,
            name: usize,
        },
        Rename {
            node: usize,
            name: usize,
        },
        Delete {
            node: usize,
        },
        Move {
            node: usize,
            folder: usize,
        },
        /// A file copied outside the app (Finder, `cp`): it repeats its original's uid.
        CopyFile {
            node: usize,
        },
    }

    fn op() -> impl Strategy<Value = Op> {
        let i = 0..64usize;
        let n = 0..NAMES.len();
        prop_oneof![
            (i.clone(), n.clone()).prop_map(|(folder, name)| Op::AddRequest { folder, name }),
            (i.clone(), n.clone()).prop_map(|(folder, name)| Op::AddFlow { folder, name }),
            (i.clone(), n.clone()).prop_map(|(folder, name)| Op::AddFolder { folder, name }),
            (i.clone(), n).prop_map(|(node, name)| Op::Rename { node, name }),
            i.clone().prop_map(|node| Op::Delete { node }),
            (i.clone(), i.clone()).prop_map(|(node, folder)| Op::Move { node, folder }),
            i.prop_map(|node| Op::CopyFile { node }),
        ]
    }

    /// Every node as (kind, uid, name, parent uid), sorted: what must survive a save and a load.
    fn shape(root: &Folder) -> Vec<(char, u64, String, u64)> {
        fn walk(f: &Folder, out: &mut Vec<(char, u64, String, u64)>) {
            for n in &f.items {
                let kind = match n {
                    Node::Folder(_) => 'd',
                    Node::Request(_) => 'r',
                    Node::Flow(_) => 'f',
                };
                out.push((kind, n.uid(), n.name().to_owned(), f.uid));
                if let Node::Folder(sub) = n {
                    walk(sub, out);
                }
            }
        }
        let mut out = Vec::new();
        walk(root, &mut out);
        out.sort();
        out
    }

    fn folders(f: &Folder, out: &mut Vec<u64>) {
        out.push(f.uid);
        for n in &f.items {
            if let Node::Folder(sub) = n {
                folders(sub, out);
            }
        }
    }

    fn nodes(f: &Folder, out: &mut Vec<u64>) {
        for n in &f.items {
            out.push(n.uid());
            if let Node::Folder(sub) = n {
                nodes(sub, out);
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// Whatever is added, renamed, moved, deleted or copied, the files hold exactly the tree.
        #[test]
        fn save_then_load_keeps_every_node(ops in proptest::collection::vec(op(), 1..14)) {
            let (_lock, tmp) = crate::storage::test_data_dir("ws_prop");
            let mut root = Folder::new("Workspace");
            let mut index = Index::default();
            save(&root, &mut index).unwrap();
            for op in ops {
                let mut all_folders = Vec::new();
                folders(&root, &mut all_folders);
                let mut all_nodes = Vec::new();
                nodes(&root, &mut all_nodes);
                let pick_folder = |i: usize| all_folders[i % all_folders.len()];
                let pick_node = |i: usize| (!all_nodes.is_empty()).then(|| all_nodes[i % all_nodes.len()]);
                match op {
                    Op::AddRequest { folder, name } => {
                        let r = Node::Request(SavedRequest { uid: new_uid(), name: NAMES[name].into(), spec: Default::default() });
                        root.folder_mut(pick_folder(folder)).unwrap().items.push(r);
                    }
                    Op::AddFlow { folder, name } => {
                        let f: Flow = serde_json::from_value(serde_json::json!({ "uid": new_uid(), "name": NAMES[name], "blocks": [] })).unwrap();
                        root.folder_mut(pick_folder(folder)).unwrap().items.push(Node::Flow(f));
                    }
                    Op::AddFolder { folder, name } => {
                        let f = Folder::new(NAMES[name]);
                        root.folder_mut(pick_folder(folder)).unwrap().items.push(Node::Folder(f));
                    }
                    Op::Rename { node, name } => {
                        if let Some(uid) = pick_node(node) {
                            root.find_mut(uid).unwrap().set_name(NAMES[name].into());
                        }
                    }
                    Op::Delete { node } => {
                        if let Some(uid) = pick_node(node) {
                            root.remove(uid);
                        }
                    }
                    Op::Move { node, folder } => {
                        if let Some(uid) = pick_node(node) {
                            let target = pick_folder(folder);
                            // Not into itself or one of its sub-folders.
                            let inside = match root.find(uid) {
                                Some(Node::Folder(f)) => { let mut v = Vec::new(); folders(f, &mut v); v.contains(&target) }
                                _ => false,
                            };
                            if !inside {
                                let n = root.remove(uid).unwrap();
                                root.folder_mut(target).unwrap().items.push(n);
                            }
                        }
                    }
                    Op::CopyFile { node } => {
                        let file = pick_node(node).and_then(|u| index.paths.get(&u).cloned());
                        if let Some(file) = file {
                            let copy = file.with_file_name(format!("copy {}", file.file_name().unwrap().to_string_lossy()));
                            fs::copy(&file, &copy).unwrap();
                            // As the app does on the outside change: read the files again.
                            let loaded = load().unwrap();
                            prop_assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
                            prop_assert!(loaded.needs_save, "the copy gets a uid of its own");
                            root = loaded.root;
                            index = loaded.index;
                        }
                    }
                }
                save(&root, &mut index).unwrap();
                let loaded = load().unwrap();
                prop_assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
                prop_assert!(!loaded.needs_save, "everything written has its uid");
                prop_assert_eq!(shape(&loaded.root), shape(&root));
                let mut uids = Vec::new();
                nodes(&loaded.root, &mut uids);
                let n = uids.len();
                uids.sort();
                uids.dedup();
                prop_assert_eq!(uids.len(), n, "uids are unique");
            }
            let _ = fs::remove_dir_all(&tmp);
        }
    }
}
