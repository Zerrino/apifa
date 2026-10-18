//! The API definitions the user imported — OpenAPI 3, Swagger 2 (JSON or YAML), or an apipie
//! export — kept as they came in `apis/` (data folder), with `apis/index.json` listing them. Together
//! they make the docs the app works from: the endpoint list, the forms, the examples, Understand,
//! the generated documentation.

use serde::{Deserialize, Serialize};

use crate::docs::ApiDoc;

const DIR: &str = "apis";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiEntry {
    pub name: String,
    /// The file in `apis/`.
    pub file: String,
    /// `openapi` or `apipie`.
    pub format: String,
    #[serde(default)]
    pub version: String,
    /// Where it was read from (a path or a URL), to read it again.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub base_urls: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub imported_at: u64,
    #[serde(default)]
    pub endpoints: usize,
}

fn yes() -> bool {
    true
}

fn dir() -> std::path::PathBuf {
    crate::storage::dir().join(DIR)
}

pub fn list() -> Vec<ApiEntry> {
    std::fs::read_to_string(dir().join("index.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_list(list: &[ApiEntry]) -> Result<(), String> {
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    crate::storage::write_atomic(&dir().join("index.json"), text.as_bytes()).map_err(|e| e.to_string())
}

/// What an imported definition holds besides its endpoints.
pub struct Parsed {
    pub doc: ApiDoc,
    pub title: String,
    pub version: String,
    pub format: &'static str,
    pub base_urls: Vec<String>,
    /// The first security scheme it declares, as the app's auth (fields to fill).
    pub auth: Option<crate::auth::Auth>,
}

/// Reads a definition: OpenAPI / Swagger first, else apipie.
pub fn parse(text: &str) -> Result<Parsed, String> {
    match crate::openapi::import(text) {
        Ok(i) => {
            let auth = i.security.first().map(|s| match s {
                crate::openapi::SecurityScheme::Basic { .. } => crate::auth::Auth::Basic { username: String::new(), password: "{{password}}".into() },
                crate::openapi::SecurityScheme::Bearer { .. } => crate::auth::Auth::Bearer { token: "{{token}}".into(), prefix: "Bearer".into() },
                crate::openapi::SecurityScheme::ApiKey { key, location, .. } => crate::auth::Auth::ApiKey {
                    key: key.clone(),
                    value: "{{api_key}}".into(),
                    location: if location == "query" { crate::auth::KeyLocation::Query } else { crate::auth::KeyLocation::Header },
                },
                crate::openapi::SecurityScheme::OAuth2 { flows, .. } => {
                    let f = flows.iter().find(|f| f.kind == "clientCredentials").or(flows.first());
                    crate::auth::Auth::OAuth2(crate::auth::OAuth2 {
                        grant: match f.map(|f| f.kind.as_str()) {
                            Some("authorizationCode") | Some("implicit") => crate::auth::Grant::AuthorizationCode,
                            Some("password") => crate::auth::Grant::Password,
                            _ => crate::auth::Grant::ClientCredentials,
                        },
                        token_url: f.map(|f| f.token_url.clone()).unwrap_or_default(),
                        auth_url: f.map(|f| f.auth_url.clone()).unwrap_or_default(),
                        client_id: "{{client_id}}".into(),
                        client_secret: "{{client_secret}}".into(),
                        scope: f.map(|f| f.scopes.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join(" ")).unwrap_or_default(),
                        ..Default::default()
                    })
                }
            });
            Ok(Parsed { doc: i.doc, title: i.title, version: i.version, format: "openapi", base_urls: i.base_urls, auth })
        }
        Err(openapi_error) => match ApiDoc::from_apipie(text) {
            Ok(doc) => Ok(Parsed { doc, title: String::new(), version: String::new(), format: "apipie", base_urls: Vec::new(), auth: None }),
            Err(_) => Err(format!("Not an API definition: {openapi_error} (OpenAPI 3, Swagger 2 or apipie, as JSON or YAML)")),
        },
    }
}

fn slug(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if s.is_empty() {
        "api".into()
    } else {
        s.chars().take(60).collect()
    }
}

/// Imports a definition (its text, and where it came from). The same name replaces the older one.
pub fn import(text: &str, source: &str, name: Option<&str>) -> Result<(ApiEntry, Parsed), String> {
    let parsed = parse(text)?;
    let fallback = std::path::Path::new(source).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "API".into());
    let name =
        name.map(str::to_owned)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| if parsed.title.trim().is_empty() { fallback } else { parsed.title.trim().to_owned() });
    let ext = if text.trim_start().starts_with('{') { "json" } else { "yaml" };
    let file = format!("{}.{ext}", slug(&name));
    std::fs::create_dir_all(dir()).map_err(|e| e.to_string())?;
    crate::storage::write_atomic(&dir().join(&file), text.as_bytes()).map_err(|e| e.to_string())?;
    let entry = ApiEntry {
        name: name.clone(),
        file,
        format: parsed.format.into(),
        version: parsed.version.clone(),
        source: source.to_owned(),
        base_urls: parsed.base_urls.clone(),
        enabled: true,
        imported_at: crate::storage::now(),
        endpoints: parsed.doc.endpoint_count,
    };
    let mut all = list();
    all.retain(|e| e.name != name);
    all.push(entry.clone());
    save_list(&all)?;
    Ok((entry, parsed))
}

pub fn remove(name: &str) -> Result<(), String> {
    let mut all = list();
    if let Some(e) = all.iter().find(|e| e.name == name) {
        let _ = std::fs::remove_file(dir().join(&e.file));
    }
    all.retain(|e| e.name != name);
    save_list(&all)
}

pub fn set_enabled(name: &str, on: bool) -> Result<(), String> {
    let mut all = list();
    all.iter_mut().filter(|e| e.name == name).for_each(|e| e.enabled = on);
    save_list(&all)
}

/// Every enabled definition as one doc. A resource key two APIs share is told apart by the API's
/// name; each resource says which API it comes from (`api`).
pub fn load_doc() -> ApiDoc {
    load_doc_of(None)
}

/// The doc of these definitions only (`None`: every enabled one) — one profile's, for Understand.
pub fn load_doc_of(names: Option<&[String]>) -> ApiDoc {
    let mut resources: Vec<crate::docs::Resource> = Vec::new();
    for e in list().into_iter().filter(|e| e.enabled && names.is_none_or(|n| n.contains(&e.name))) {
        let Ok(text) = std::fs::read_to_string(dir().join(&e.file)) else {
            crate::storage::report(format!("The API definition “{}” is missing its file ({})", e.name, e.file));
            continue;
        };
        match parse(&text) {
            Ok(p) => {
                for mut r in p.doc.resources {
                    if resources.iter().any(|x| x.key == r.key) {
                        r.key = format!("{}_{}", slug(&e.name).replace('-', "_"), r.key);
                    }
                    r.api = e.name.clone();
                    resources.push(r);
                }
            }
            Err(err) => crate::storage::report(format!("The API definition “{}” can't be read: {err}", e.name)),
        }
    }
    ApiDoc::from_resources(resources)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_list_merge_remove() {
        let (_lock, _dir) = crate::storage::test_data_dir("apis");
        let petstore = r#"{"openapi":"3.0.0","info":{"title":"Pets","version":"1"},"servers":[{"url":"https://pets.example/v1"}],
            "paths":{"/pets":{"get":{"tags":["pets"],"operationId":"listPets","responses":{"200":{"description":"ok"}}}}},
            "components":{"securitySchemes":{"key":{"type":"apiKey","name":"X-Key","in":"header"}}}}"#;
        let (e, p) = import(petstore, "/tmp/petstore.json", None).unwrap();
        assert_eq!((e.name.as_str(), e.format.as_str(), e.endpoints), ("Pets", "openapi", 1));
        assert_eq!(e.base_urls, ["https://pets.example/v1"]);
        assert!(matches!(p.auth, Some(crate::auth::Auth::ApiKey { ref key, .. }) if key == "X-Key"));
        // A second API with the same resource name: both kept.
        import(&petstore.replace("\"Pets\"", "\"Pets 2\""), "x", None).unwrap();
        let doc = load_doc();
        assert_eq!(doc.endpoint_count, 2);
        assert_eq!(doc.resources.len(), 2);
        set_enabled("Pets 2", false).unwrap();
        assert_eq!(load_doc().endpoint_count, 1);
        remove("Pets").unwrap();
        assert_eq!(list().len(), 1);
        assert!(parse("{\"nope\": 1}").is_err());
        // apipie works too.
        assert_eq!(parse(include_str!("../tests/fixtures/apidoc.json")).unwrap().format, "apipie");
    }
}
