//! API profiles: one per API you work with (like a Postman / Bruno collection) — its name, its base
//! URL, its own authentication (so its own tokens), the API definitions that describe it, its own
//! data map. Folders and requests say which profile they belong to; the active one (title bar) is
//! used by the rest. Saved in `profiles.json`, next to `workspace/`.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::auth::Auth;

pub const FILE: &str = "profiles.json";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// Stable id (`pets-api`): what folders and requests point to, the data map's file name.
    pub id: String,
    pub name: String,
    /// Where its relative paths go (templates allowed: `{{petsUrl}}`); empty = the environment's
    /// `baseUrl`, else the workspace's.
    #[serde(default)]
    pub base_url: String,
    /// Its authentication (`Inherit`: the workspace's).
    #[serde(default)]
    pub auth: Auth,
    /// The imported API definitions (`apis.rs` names) that describe it.
    #[serde(default)]
    pub apis: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Profiles {
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub list: Vec<Profile>,
}

/// `Pets API` → `pets-api`.
pub fn slug(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() {
        "api".into()
    } else {
        s.chars().take(48).collect()
    }
}

impl Profiles {
    pub fn load() -> Self {
        crate::storage::load(FILE)
    }

    pub fn save(&self) {
        crate::storage::save(FILE, self);
    }

    pub fn get(&self, id: &str) -> Option<&Profile> {
        self.list.iter().find(|p| p.id == id)
    }

    pub fn active(&self) -> Option<&Profile> {
        self.get(self.active.as_deref()?)
    }

    /// A new profile named `name` (its id made unique); returns the id.
    pub fn add(&mut self, mut p: Profile) -> String {
        let base = slug(if p.id.is_empty() { &p.name } else { &p.id });
        let mut id = base.clone();
        let mut n = 2;
        while self.get(&id).is_some() {
            id = format!("{base}-{n}");
            n += 1;
        }
        p.id = id.clone();
        self.list.push(p);
        id
    }

    /// The profile an API definition belongs to.
    pub fn of_api(&self, api: &str) -> Option<&Profile> {
        self.list.iter().find(|p| p.apis.iter().any(|a| a == api))
    }

    /// Makes `id` the active one (`None`: none).
    pub fn select(&mut self, id: Option<&str>) -> Result<(), String> {
        match id {
            None => self.active = None,
            Some(i) if self.get(i).is_some() => self.active = Some(i.to_owned()),
            Some(i) => {
                let ids: Vec<&str> = self.list.iter().map(|p| p.id.as_str()).collect();
                return Err(format!("No profile “{i}” (there are: {})", if ids.is_empty() { "none".into() } else { ids.join(", ") }));
            }
        }
        Ok(())
    }

    /// The id for a name or an id (the CLI's `--profile`).
    pub fn find(&self, name_or_id: &str) -> Option<&Profile> {
        let n = name_or_id.trim();
        self.get(n).or_else(|| self.list.iter().find(|p| p.name.eq_ignore_ascii_case(n)))
    }
}

/// After an import: the profile of that API (made when there is none — its first http server as base
/// URL, the auth it declares), its id.
pub fn profile_for_import(ps: &mut Profiles, entry: &crate::apis::ApiEntry, auth: Option<Auth>) -> String {
    let base = entry.base_urls.iter().find(|b| b.starts_with("http")).cloned().unwrap_or_default();
    if let Some(p) = ps.list.iter_mut().find(|p| p.apis.contains(&entry.name)) {
        if p.base_url.trim().is_empty() {
            p.base_url = base;
        }
        return p.id.clone();
    }
    ps.add(Profile {
        name: entry.name.clone(),
        base_url: base,
        auth: auth.unwrap_or_default(),
        apis: vec![entry.name.clone()],
        description: String::new(),
        id: String::new(),
    })
}

/// The profiles requests and flows use now, and the active one; set by the app and the CLI.
static CURRENT: RwLock<Option<Profiles>> = RwLock::new(None);

pub fn set_current(p: &Profiles) {
    *CURRENT.write() = Some(p.clone());
}

/// The profile `id` (empty: the active one).
pub fn current(id: &str) -> Option<Profile> {
    let all = CURRENT.read();
    let all = all.as_ref()?;
    if id.trim().is_empty() {
        all.active().cloned()
    } else {
        all.get(id).cloned()
    }
}

/// The id of the profile an API definition belongs to (empty: none).
pub fn of_api_id(api: &str) -> String {
    CURRENT.read().as_ref().and_then(|p| p.of_api(api).map(|x| x.id.clone())).unwrap_or_default()
}

/// Every profile, as (id, name), for pickers.
pub fn all() -> Vec<(String, String)> {
    CURRENT.read().as_ref().map(|p| p.list.iter().map(|x| (x.id.clone(), x.name.clone())).collect()).unwrap_or_default()
}

/// The active profile's id (empty: none).
pub fn active_id() -> String {
    CURRENT.read().as_ref().and_then(|p| p.active.clone()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_find_select() {
        let mut ps = Profiles::default();
        let a = ps.add(Profile { name: "Pets API".into(), apis: vec!["Pets".into()], ..Default::default() });
        let b = ps.add(Profile { name: "Pets API".into(), ..Default::default() });
        assert_eq!((a.as_str(), b.as_str()), ("pets-api", "pets-api-2"));
        assert_eq!(ps.of_api("Pets").unwrap().id, "pets-api");
        assert_eq!(ps.find("pets api").unwrap().id, "pets-api");
        assert!(ps.select(Some("nope")).is_err());
        ps.select(Some(&b)).unwrap();
        assert_eq!(ps.active().unwrap().id, "pets-api-2");
        let json = serde_json::to_string(&ps).unwrap();
        let back: Profiles = serde_json::from_str(&json).unwrap();
        assert_eq!(back.list, ps.list);
    }

    /// Two APIs, two tokens: each request goes to its profile's base URL with its profile's auth.
    #[test]
    fn each_profile_its_base_url_and_token() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for s in listener.incoming() {
                let mut s = s.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).unwrap();
                seen2.lock().push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
                let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}");
            }
        });
        let bearer = |t: &str| Auth::Bearer { token: t.into(), prefix: "Bearer".into() };
        let mut ps = Profiles::default();
        let pets = ps.add(Profile { name: "Pets".into(), base_url: format!("{base}/pets-api"), auth: bearer("pets-token"), ..Default::default() });
        let shop = ps.add(Profile { name: "Shop".into(), base_url: format!("{base}/shop-api"), auth: bearer("shop-token"), ..Default::default() });
        // No active profile: other tests running meanwhile see no default (the global is shared).
        set_current(&ps);
        let send = |profile: &str| {
            let spec = crate::model::RequestSpec { path: "/items".into(), profile: profile.into(), ..Default::default() };
            let p = crate::model::prepare(&spec, None);
            crate::net::execute(&crate::net::agent(), &Default::default(), &crate::net::Credentials::default(), &p).unwrap();
        };
        send(&pets);
        send(&shop);
        let seen = seen.lock();
        assert!(seen[0].starts_with("get /pets-api/items") && seen[0].contains("authorization: bearer pets-token"), "{}", seen[0]);
        assert!(seen[1].starts_with("get /shop-api/items") && seen[1].contains("authorization: bearer shop-token"), "{}", seen[1]);
        set_current(&Profiles::default());
    }
}
