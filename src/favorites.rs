//! Favorites: starred endpoints / workspace items, optionally organised in folders.
//! Stored in `favorites.json` as a list (older flat lists load unchanged).

use serde::{Deserialize, Serialize};

use crate::model::DocRef;
use crate::workspace::new_uid;

/// A starred endpoint or workspace item.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Favorite {
    Endpoint(DocRef),
    Item { uid: u64 },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    Endpoint(DocRef),
    Item {
        uid: u64,
    },
    Folder {
        #[serde(default = "new_uid")]
        uid: u64,
        name: String,
        #[serde(default)]
        items: Vec<Entry>,
    },
}

impl Entry {
    pub fn favorite(&self) -> Option<Favorite> {
        match self {
            Entry::Endpoint(d) => Some(Favorite::Endpoint(d.clone())),
            Entry::Item { uid } => Some(Favorite::Item { uid: *uid }),
            Entry::Folder { .. } => None,
        }
    }

    fn folder_uid(&self) -> Option<u64> {
        match self {
            Entry::Folder { uid, .. } => Some(*uid),
            _ => None,
        }
    }

    fn from(f: Favorite) -> Self {
        match f {
            Favorite::Endpoint(d) => Entry::Endpoint(d),
            Favorite::Item { uid } => Entry::Item { uid },
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Favorites {
    pub items: Vec<Entry>,
}

fn contains_in(items: &[Entry], f: &Favorite) -> bool {
    items.iter().any(|e| match e {
        Entry::Folder { items, .. } => contains_in(items, f),
        other => other.favorite().as_ref() == Some(f),
    })
}

/// Takes the entry out of the tree (`pred` matches it), wherever it is.
fn take(items: &mut Vec<Entry>, pred: &dyn Fn(&Entry) -> bool) -> Option<Entry> {
    if let Some(i) = items.iter().position(pred) {
        return Some(items.remove(i));
    }
    items.iter_mut().find_map(|e| match e {
        Entry::Folder { items, .. } => take(items, pred),
        _ => None,
    })
}

fn folder_items(items: &mut [Entry], uid: u64) -> Option<&mut Vec<Entry>> {
    for e in items.iter_mut() {
        if let Entry::Folder { uid: u, items: inner, .. } = e {
            if *u == uid {
                return Some(inner);
            }
            if let Some(found) = folder_items(inner, uid) {
                return Some(found);
            }
        }
    }
    None
}

impl Favorites {
    pub fn contains(&self, f: &Favorite) -> bool {
        contains_in(&self.items, f)
    }

    /// The list of `folder`, or the top level for `None` / an unknown folder.
    fn list(&mut self, folder: Option<u64>) -> &mut Vec<Entry> {
        let exists = folder.is_some_and(|uid| folder_items(&mut self.items, uid).is_some());
        match folder {
            Some(uid) if exists => folder_items(&mut self.items, uid).expect("checked above"),
            _ => &mut self.items,
        }
    }

    pub fn toggle(&mut self, f: Favorite) {
        if !self.remove(&f) {
            self.items.push(Entry::from(f));
        }
    }

    pub fn remove(&mut self, f: &Favorite) -> bool {
        take(&mut self.items, &|e| e.favorite().as_ref() == Some(f)).is_some()
    }

    /// Adds `f` to `folder`, moving it there if it was already starred.
    pub fn add_to(&mut self, f: Favorite, folder: Option<u64>) {
        self.remove(&f);
        self.list(folder).push(Entry::from(f));
    }

    pub fn add_folder(&mut self, parent: Option<u64>, name: &str) {
        let name = if name.trim().is_empty() { "New folder" } else { name.trim() };
        self.list(parent).push(Entry::Folder { uid: new_uid(), name: name.to_owned(), items: Vec::new() });
    }

    pub fn rename_folder(&mut self, uid: u64, name: &str) {
        fn walk(items: &mut [Entry], uid: u64, name: &str) {
            for e in items {
                if let Entry::Folder { uid: u, name: n, items } = e {
                    if *u == uid {
                        *n = name.to_owned();
                        return;
                    }
                    walk(items, uid, name);
                }
            }
        }
        if !name.trim().is_empty() {
            walk(&mut self.items, uid, name.trim());
        }
    }

    /// Removes the folder; its favorites move up to where the folder was.
    pub fn delete_folder(&mut self, uid: u64) {
        fn walk(items: &mut Vec<Entry>, uid: u64) -> bool {
            if let Some(i) = items.iter().position(|e| e.folder_uid() == Some(uid)) {
                if let Entry::Folder { items: inner, .. } = items.remove(i) {
                    for (k, child) in inner.into_iter().enumerate() {
                        items.insert(i + k, child);
                    }
                }
                return true;
            }
            items.iter_mut().any(|e| match e {
                Entry::Folder { items, .. } => walk(items, uid),
                _ => false,
            })
        }
        walk(&mut self.items, uid);
    }

    /// Moves a folder; refuses to put it inside itself.
    pub fn move_folder(&mut self, uid: u64, to: Option<u64>) -> bool {
        if let Some(target) = to {
            if target == uid || folder_items(&mut self.items, uid).is_some_and(|inner| folder_items(inner, target).is_some()) {
                return false;
            }
        }
        match take(&mut self.items, &|e| e.folder_uid() == Some(uid)) {
            Some(folder) => {
                self.list(to).push(folder);
                true
            }
            None => false,
        }
    }

    /// Every folder as (uid, "A / B"), the top level first as `None`.
    pub fn folder_paths(&self) -> Vec<(Option<u64>, String)> {
        fn walk(items: &[Entry], prefix: &str, out: &mut Vec<(Option<u64>, String)>) {
            for e in items {
                if let Entry::Folder { uid, name, items } = e {
                    let path = if prefix.is_empty() { name.clone() } else { format!("{prefix} / {name}") };
                    out.push((Some(*uid), path.clone()));
                    walk(items, &path, out);
                }
            }
        }
        let mut out = vec![(None, "(top level)".to_owned())];
        walk(&self.items, "", &mut out);
        out
    }

    pub fn folder_name(&self, uid: u64) -> Option<String> {
        self.folder_paths().into_iter().find(|(u, _)| *u == Some(uid)).map(|(_, p)| p.rsplit(" / ").next().unwrap_or_default().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(m: &str) -> Favorite {
        Favorite::Endpoint(DocRef { resource: "users".into(), method: m.into(), route: 0 })
    }

    #[test]
    fn old_flat_files_still_load() {
        let old = r#"[{"kind":"endpoint","resource":"users","method":"show","route":0},{"kind":"item","uid":7}]"#;
        let f: Favorites = serde_json::from_str(old).unwrap();
        assert!(f.contains(&ep("show")) && f.contains(&Favorite::Item { uid: 7 }));
    }

    #[test]
    fn folders() {
        let mut f = Favorites::default();
        f.toggle(ep("show"));
        f.add_folder(None, "Users");
        let users = f.folder_paths()[1].0;
        f.add_folder(users, "Admin");
        let admin = f.folder_paths()[2].0;
        f.add_to(ep("show"), admin);
        assert_eq!(f.items.len(), 1, "moved out of the top level");
        assert!(f.contains(&ep("show")));
        assert!(!f.move_folder(users.unwrap(), admin), "not inside itself");
        f.delete_folder(users.unwrap());
        assert_eq!(f.folder_paths().len(), 2, "Admin moved up");
        f.toggle(ep("show"));
        assert!(!f.contains(&ep("show")));
        let back: Favorites = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back.folder_paths().len(), 2);
    }
}
