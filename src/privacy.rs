//! What AIs see (the assistant's provider, MCP clients): the structure of the data, never a value.
//!
//! Every value becomes its type, plus a keyed fingerprint for the ones that can repeat:
//! `{"login": "jdoe", "id": 7}` → `{"login": "string#5d1e…", "id": "integer#a03f…"}`. The same value
//! gives the same fingerprint, so repetitions and links stay visible (the `user_id` of a row is the
//! `id` of that user); the fingerprint is an HMAC-SHA256 with a key made on this computer
//! (`anonymization.key` in the data folder), so it can't be reversed by hashing guesses
//! (a plain SHA of a login is found back from a list of logins in no time).
//!
//! Strings also say their format when it is one (`date-time`, `email`, `url`…), booleans only say
//! `boolean` (two values, a fingerprint would give them away), `null` stays `null`.
//!
//! The values given as fingerprints are kept in memory (never on disk) so that what the AI sends back
//! works: a tool input with `string#5d1e…` gets `jdoe` again on this computer, just before it's used
//! (`reveal`). Logins, names and emails seen this way are also hidden in the free text that goes to
//! the AI (the user's chat messages, the names and notes of saved items: `mask_text`).

use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Items of a list shown (the next ones only counted): the shape, not the volume.
const LIST_ITEMS: usize = 5;

/// Tests never touch the data folder: a fixed key.
#[cfg(test)]
fn key() -> &'static [u8; 32] {
    &[7; 32]
}

#[cfg(not(test))]
fn key() -> &'static [u8; 32] {
    use std::sync::OnceLock;
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    KEY.get_or_init(|| {
        let path = crate::storage::dir().join("anonymization.key");
        if let Some(k) = std::fs::read_to_string(&path).ok().and_then(|t| decode_hex(t.trim())) {
            return k;
        }
        let k = random_key();
        let _ = std::fs::create_dir_all(crate::storage::dir());
        // Readable by the user only, like the config.
        #[cfg(unix)]
        {
            use std::io::Write;
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut f) = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path) {
                let _ = f.write_all(hex(&k).as_bytes());
            }
        }
        #[cfg(not(unix))]
        let _ = std::fs::write(&path, hex(&k));
        k
    })
}

/// 256 bits from the system's randomness (std's hasher keys come from it).
fn random_key() -> [u8; 32] {
    use std::hash::{BuildHasher, Hasher};
    let mut k = [0u8; 32];
    for (i, chunk) in k.chunks_mut(8).enumerate() {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    k
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn decode_hex(s: &str) -> Option<[u8; 32]> {
    let mut out = [0u8; 32];
    if s.len() != 64 {
        return None;
    }
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// HMAC-SHA256 of `value` with the local key.
fn hmac(value: &[u8]) -> [u8; 32] {
    let k = key();
    let (mut ipad, mut opad) = ([0x36u8; 64], [0x5cu8; 64]);
    for i in 0..32 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new().chain_update(ipad).chain_update(value).finalize();
    Sha256::new().chain_update(opad).chain_update(inner).finalize().into()
}

/// A short fingerprint of a value (10 hex digits): equal values, equal fingerprints.
pub fn fingerprint(value: &str) -> String {
    hex(&hmac(value.as_bytes())[..5])
}

/// The same, as a number (to compare sets of values without keeping them).
pub fn fingerprint_u64(value: &str) -> u64 {
    let h = hmac(value.as_bytes());
    u64::from_le_bytes(h[..8].try_into().expect("8 bytes"))
}

/// What a string holds, when it's a known format.
pub fn string_format(s: &str) -> Option<&'static str> {
    let t = s.trim();
    if t.is_empty() {
        return Some("empty");
    }
    if chrono::DateTime::parse_from_rfc3339(t).is_ok() {
        return Some("date-time");
    }
    if chrono::NaiveDate::parse_from_str(t, "%Y-%m-%d").is_ok() {
        return Some("date");
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        return Some("url");
    }
    if t.contains('@') && t.contains('.') && !t.contains(' ') {
        return Some("email");
    }
    if t.len() == 36 && t.chars().filter(|&c| c == '-').count() == 4 && t.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Some("uuid");
    }
    if t.parse::<f64>().is_ok() {
        return Some("numeric");
    }
    None
}

/// When an object is a dictionary whose keys are data (`{"2017-11-13": 4}`, `{"jdoe": 3}`), the name for
/// its keys: `{date}`, `{number}`, `{key}`… Its keys are then values: never shown.
pub fn data_keys(o: &Map<String, Value>) -> Option<&'static str> {
    if o.is_empty() {
        return None;
    }
    // Every key has the format of a value.
    let formats: HashSet<Option<&'static str>> = o.keys().map(|k| string_format(k)).collect();
    if formats.len() == 1 {
        if let Some(Some(f)) = formats.into_iter().next().filter(|f| f.is_some_and(|f| f != "empty")) {
            return Some(match f {
                "date" => "{date}",
                "date-time" => "{date-time}",
                "numeric" => "{number}",
                "uuid" => "{uuid}",
                "email" => "{email}",
                _ => "{key}",
            });
        }
    }
    // Many keys, all holding the same kind of value: a lookup table (logins → counts).
    let kind = |v: &Value| match v {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    };
    let kinds: HashSet<u8> = o.values().map(kind).collect();
    if kinds.len() != 1 {
        return None;
    }
    // A few keys already, when one of them can't be a field name (`jean-luc`, `Jdoe`, `42born2code`) and
    // the values are numbers or objects (a response's headers, all text, are names).
    let field_like = |k: &str| {
        let k = k.strip_suffix('?').unwrap_or(k);
        k.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    let counts_or_records = kinds.contains(&2) || kinds.contains(&5);
    (o.len() > 12 || (o.len() >= 2 && counts_or_records && o.keys().any(|k| !field_like(k)))).then_some("{key}")
}

/// Fingerprint → value of what the AI was given this session, and the ones that are about a person.
#[derive(Default)]
struct Vault {
    values: HashMap<String, String>,
    personal: HashSet<String>,
}

static VAULT: LazyLock<Mutex<Vault>> = LazyLock::new(Mutex::default);

/// Kept at most (a few MB): past that, new values can't be revealed (the AI is told to use the real one).
const VAULT_MAX: usize = 200_000;

/// Words of a key whose values identify a person (`login`, `usual_first_name`, `phone`…): hidden in
/// free text too. Plain words of personal data, about no API in particular.
const PERSONAL_WORDS: [&str; 9] = ["login", "email", "mail", "phone", "first_name", "last_name", "full_name", "displayname", "username"];

fn personal_key(k: &str) -> bool {
    let k = k.to_lowercase();
    PERSONAL_WORDS.iter().any(|w| k == *w || k.ends_with(&format!("_{w}")) || k.starts_with(&format!("{w}_")) || (w.contains('_') && k.contains(w)))
}

fn vault() -> std::sync::MutexGuard<'static, Vault> {
    VAULT.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn remember(fp: &str, value: &str, personal: bool) {
    let mut v = vault();
    if v.values.len() < VAULT_MAX && !v.values.contains_key(fp) {
        v.values.insert(fp.to_owned(), value.to_owned());
    }
    if personal && v.personal.len() < VAULT_MAX {
        v.personal.insert(fp.to_owned());
    }
}

/// Logins, names, emails of a response the app received: hidden from then on in what the user writes
/// to the AI (`mask_text`) and in the keys of the dictionaries the AI is given.
pub fn learn(v: &Value) {
    let mut found = Vec::new();
    collect_personal(v, None, &mut found);
    for value in found {
        remember(&fingerprint(&value), &value, true);
    }
}

/// `learn` on a response body (text).
pub fn learn_body(body: &str) {
    let t = body.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            learn(&v);
        }
    }
}

fn collect_personal(v: &Value, key: Option<&str>, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => o.iter().for_each(|(k, child)| collect_personal(child, Some(k), out)),
        Value::Array(a) => a.iter().for_each(|x| collect_personal(x, key, out)),
        Value::String(s) if !s.trim().is_empty() && (key.is_some_and(personal_key) || string_format(s) == Some("email")) => out.push(s.clone()),
        _ => {}
    }
}

/// Whether `word` is a login / name / email the app has seen.
fn is_personal(word: &str) -> bool {
    let v = vault();
    !v.personal.is_empty() && v.personal.contains(&fingerprint(word))
}

/// Free text going to the AI (a chat message, the name or the note of a saved item): the logins, names
/// and emails the app has seen, and every email, become fingerprints the AI can use as is (`reveal`).
pub fn mask_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        let core = word.trim_end_matches(['.', '-', '_']);
        let email = string_format(core) == Some("email");
        if !core.is_empty() && (email || is_personal(core)) {
            if email {
                remember(&fingerprint(core), core, true);
            }
            out.push_str(scalar(&Value::from(core)).as_str().unwrap_or_default());
            out.push_str(&word[core.len()..]);
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | '+') {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// What the AI sends back (a tool input), with the fingerprints it was given turned back into their
/// values: `/v1/users/<string#5d1e…>` → `/v1/users/jdoe`. Unknown fingerprints stay as they are.
pub fn reveal(v: &Value) -> Value {
    match v {
        Value::String(s) => Value::from(reveal_text(s)),
        Value::Array(a) => Value::Array(a.iter().map(reveal).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (reveal_text(k), reveal(v))).collect()),
        other => other.clone(),
    }
}

/// `reveal` in a text: each `type#fingerprint` (`string#…`, `string (email)#…`, `integer#…`, `{key}#…`,
/// also between `<…>`) whose value is known.
pub fn reveal_text(s: &str) -> String {
    if !s.contains('#') {
        return s.to_owned();
    }
    let v = vault();
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('#') {
        let fp = rest[at + 1..].get(..10).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
        let after_ok = rest.as_bytes()[at + 1..].get(10).is_none_or(|b| !b.is_ascii_hexdigit());
        let before = &rest[..at];
        // The type written before the `#`.
        let prefix = ["string", "integer", "number", "}"].iter().find_map(|t| before.strip_suffix(t).map(|b| (b, *t))).or_else(|| {
            // `string (email)`
            let inner = before.strip_suffix(')')?;
            let open = inner.rfind("string (")?;
            inner[open + 8..].chars().all(|c| c.is_ascii_lowercase() || c == '-').then(|| (&before[..open], ""))
        });
        match (fp, after_ok, prefix) {
            (Some(fp), true, Some((mut head, t))) if v.values.contains_key(fp) => {
                if t == "}" {
                    head = head.rfind('{').map_or(head, |i| &head[..i]);
                }
                let mut tail = &rest[at + 11..];
                if head.ends_with('<') && tail.starts_with('>') {
                    head = &head[..head.len() - 1];
                    tail = &tail[1..];
                }
                out.push_str(head);
                out.push_str(&v.values[fp]);
                rest = tail;
            }
            _ => {
                out.push_str(&rest[..=at]);
                rest = &rest[at + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One value's type and fingerprint, as a string (`"integer#…"`, `"string (date-time)#…"`).
fn scalar(v: &Value) -> Value {
    match v {
        Value::Null => Value::Null,
        Value::Bool(_) => Value::from("boolean"),
        Value::Number(n) => {
            let kind = if n.is_i64() || n.is_u64() { "integer" } else { "number" };
            let text = n.to_string();
            let fp = fingerprint(&text);
            remember(&fp, &text, false);
            Value::from(format!("{kind}#{fp}"))
        }
        Value::String(s) => {
            let format = string_format(s);
            if format == Some("empty") {
                return Value::from("string (empty)");
            }
            let fp = fingerprint(s);
            remember(&fp, s, format == Some("email"));
            match format {
                Some(f) => Value::from(format!("string ({f})#{fp}")),
                None => Value::from(format!("string#{fp}")),
            }
        }
        _ => unreachable!("scalars only"),
    }
}

/// The JSON with every value replaced by its type and fingerprint; the keys and the nesting stay.
/// A list keeps its first items and says how many more there are.
pub fn json(v: &Value) -> Value {
    learn(v);
    anon(v)
}

fn anon(v: &Value) -> Value {
    match v {
        // A dictionary keyed by data: its keys are values too.
        Value::Object(o) if data_keys(o).is_some() => {
            let label = data_keys(o).unwrap_or("{key}");
            let mut out: Map<String, Value> = o.iter().take(LIST_ITEMS).map(|(k, v)| (data_key(label, k), anon(v))).collect();
            if o.len() > LIST_ITEMS {
                out.insert("…".into(), Value::from(format!("{} more keys", o.len() - LIST_ITEMS)));
            }
            Value::Object(out)
        }
        // A key that is a login the app has seen (`{"jdoe": 3}`): a value too.
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (if is_personal(k) { data_key("{key}", k) } else { k.clone() }, anon(v))).collect()),
        Value::Array(a) => {
            let mut out: Vec<Value> = a.iter().take(LIST_ITEMS).map(anon).collect();
            if a.len() > LIST_ITEMS {
                out.push(Value::from(format!("… {} more items", a.len() - LIST_ITEMS)));
            }
            Value::Array(out)
        }
        scalar_value => scalar(scalar_value),
    }
}

fn data_key(label: &str, k: &str) -> String {
    let fp = fingerprint(k);
    remember(&fp, k, false);
    format!("{label}#{fp}")
}

/// A response body: its JSON anonymized, else what kind of text it is (never the text).
pub fn body(body: &str) -> String {
    let t = body.trim();
    if t.is_empty() {
        return "(empty body)".into();
    }
    match serde_json::from_str::<Value>(t) {
        Ok(v) => serde_json::to_string_pretty(&json(&v)).unwrap_or_default(),
        Err(_) => format!("(not JSON: {} characters of text #{})", t.chars().count(), fingerprint(t)),
    }
}

/// Literal path segments of the documented routes (`v1`, `users`, `orders`…): structure, not data.
pub fn vocabulary(doc: &crate::docs::ApiDoc) -> HashSet<String> {
    let mut words = HashSet::new();
    for res in &doc.resources {
        for m in &res.methods {
            for r in &m.routes {
                for seg in r.path.split('/') {
                    if !seg.is_empty() && !seg.starts_with(':') {
                        words.insert(seg.to_owned());
                    }
                }
            }
        }
    }
    words
}

/// A path with its values replaced: segments that are words of the API (`/v1/users/…`), `:variables`
/// and `{{templates}}` stay, the others (`jdoe`, `17`) become fingerprints; query values too.
pub fn path(path: &str, vocabulary: &HashSet<String>) -> String {
    let (p, query) = path.split_once('?').unwrap_or((path, ""));
    let segs: Vec<String> = p
        .split('/')
        .map(|seg| {
            if seg.is_empty() || seg.starts_with(':') || vocabulary.contains(seg) {
                seg.to_owned()
            } else if seg.contains("{{") {
                templated(seg)
            } else {
                format!("<{}>", scalar(&serde_json::from_str::<Value>(seg).ok().filter(Value::is_number).unwrap_or_else(|| Value::from(seg))).as_str().unwrap_or("value"))
            }
        })
        .collect();
    let mut out = segs.join("/");
    if !query.is_empty() {
        let q: Vec<String> = query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((k, v)) => format!("{k}={}", text_value(v)),
                None => pair.to_owned(),
            })
            .collect();
        out.push('?');
        out.push_str(&q.join("&"));
    }
    out
}

/// A text value typed somewhere (a query value, a field of a saved request): templates (`{{login}}`,
/// `{{now-7d}}`) are kept, they're how the request works; anything else is a value, also around a
/// template (`jdoe,{{x}}` → `<string#…>,{{x}}`).
pub fn text_value(v: &str) -> String {
    if v.is_empty() {
        return String::new();
    }
    if v.contains("{{") {
        return templated(v);
    }
    plain_value(v)
}

/// A literal value (no template): its type and fingerprint.
fn plain_value(v: &str) -> String {
    match serde_json::from_str::<Value>(v.trim()) {
        Ok(n @ Value::Number(_)) => scalar(&n).as_str().unwrap_or_default().to_owned(),
        Ok(Value::Bool(_)) => "boolean".into(),
        _ => scalar(&Value::from(v)).as_str().unwrap_or_default().to_owned(),
    }
}

/// Text mixing `{{templates}}` and literal values: the templates stay, every literal word between
/// them becomes `<its type#fingerprint>`; separators (`,` `:` spaces…) stay.
fn templated(v: &str) -> String {
    fn literal(piece: &str, out: &mut String) {
        let mut word = String::new();
        let flush = |word: &mut String, out: &mut String| {
            // `jdoe:` → `<…>:`
            let core = word.trim_end_matches([':', ';', '!', '?', ')', '.']);
            if core.chars().any(char::is_alphanumeric) {
                out.push('<');
                out.push_str(&plain_value(core));
                out.push('>');
                out.push_str(&word[core.len()..]);
            } else {
                out.push_str(word);
            }
            word.clear();
        };
        for c in piece.chars() {
            if c == ',' || c.is_whitespace() {
                flush(&mut word, out);
                out.push(c);
            } else {
                word.push(c);
            }
        }
        flush(&mut word, out);
    }
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(start) = rest.find("{{") {
        literal(&rest[..start], &mut out);
        match rest[start..].find("}}") {
            Some(end) => {
                out.push_str(&rest[start..start + end + 2]);
                rest = &rest[start + end + 2..];
            }
            None => {
                literal(&rest[start..], &mut out);
                rest = "";
            }
        }
    }
    literal(rest, &mut out);
    out
}

/// A JSON body (text) with `{{templates}}` in it (`{"login": "jdoe", "n": {{n}}}`): anonymized as JSON,
/// the templates kept where they are. Not JSON even so: anonymized as text.
fn templated_body(b: &str) -> String {
    // Each template becomes a marker JSON can parse: a string of its own outside quotes (U+0001),
    // a piece of the string inside (U+0002).
    let mut src = String::with_capacity(b.len());
    let mut templates: Vec<&str> = Vec::new();
    let (mut in_str, mut escaped) = (false, false);
    let mut i = 0;
    while i < b.len() {
        let rest = &b[i..];
        if rest.starts_with("{{") {
            if let Some(end) = rest.find("}}") {
                let n = templates.len();
                templates.push(&rest[..end + 2]);
                src.push_str(&if in_str { format!("\\u0002{n}\\u0002") } else { format!("\"\\u0001{n}\\u0001\"") });
                i += end + 2;
                escaped = false;
                continue;
            }
        }
        let c = rest.chars().next().expect("not at the end");
        if in_str {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_str = false,
                _ => {}
            }
        } else if c == '"' {
            in_str = true;
        }
        src.push(c);
        i += c.len_utf8();
    }
    let Ok(parsed) = serde_json::from_str::<Value>(&src) else { return text_value(b) };
    let restore = |s: &str| {
        let mut out = s.to_owned();
        for (n, t) in templates.iter().enumerate() {
            out = out.replace(&format!("\u{2}{n}\u{2}"), t);
        }
        out
    };
    fn walk(v: &Value, restore: &dyn Fn(&str) -> String) -> Value {
        match v {
            // A template on its own: kept as the marker, put back after.
            Value::String(s) if s.starts_with('\u{1}') => v.clone(),
            Value::String(s) if s.contains('\u{2}') => Value::from(templated(&restore(s))),
            Value::Object(o) if data_keys(o).is_none() => Value::Object(o.iter().map(|(k, v)| (restore(k), walk(v, restore))).collect()),
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, restore)).collect()),
            other => json(other),
        }
    }
    let mut out = walk(&parsed, &restore).to_string();
    for (n, t) in templates.iter().enumerate() {
        out = out.replace(&format!("\"\\u0001{n}\\u0001\""), t);
    }
    out
}

/// A message (an error, a note) with the values it quotes (“jdoe”) replaced by their fingerprints.
pub fn message(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('“') {
        out.push_str(&rest[..start]);
        let after = &rest[start + '“'.len_utf8()..];
        let Some(end) = after.find('”') else {
            out.push_str(&rest[start..]);
            return out;
        };
        out.push('“');
        out.push_str(&text_value(&after[..end]));
        out.push('”');
        rest = &after[end + '”'.len_utf8()..];
    }
    out.push_str(rest);
    out
}

/// Keys whose text is how a saved request or a flow works (names, verbs, variable names, operators),
/// not data: kept. Every other text is a value (anonymized, unless it's a `{{template}}`).
const STRUCTURE_KEYS: [&str; 24] = [
    "name",
    "type",
    "kind",
    "verb",
    "save_as",
    "from",
    "into",
    "list",
    "item",
    "op",
    "expr",
    "key",
    "body_mode",
    "resource",
    "method",
    "description",
    "note",
    "title",
    "flow",
    "when",
    "extract",
    "fields",
    "uid",
    "route",
];

/// Structure keys whose text the user wrote freely.
const FREE_TEXT: [&str; 4] = ["name", "description", "note", "title"];

/// Maps of name → value (path variables, documented fields, a flow block's inputs): every value is one,
/// whatever its name (a path variable can be called `name`).
const VALUE_MAPS: [&str; 3] = ["path_values", "values", "inputs"];

/// A saved request or flow (as JSON) with its values anonymized: the user's literal values (a login
/// in a path, `filter[store_id]=12`, a body, an input's value, enum options) become fingerprints;
/// the structure, the names and the `{{templates}}` stay.
pub fn saved(v: &Value, vocabulary: &HashSet<String>) -> Value {
    fn walk(v: &Value, key: Option<&str>, vocabulary: &HashSet<String>) -> Value {
        match v {
            Value::Object(o) => {
                let mut out = Map::new();
                for (k, child) in o {
                    let anon = match (k.as_str(), child) {
                        ("path", Value::String(p)) => Value::from(path(p, vocabulary)),
                        // A JSON body (text): anonymized as JSON when it is, templates kept.
                        ("raw_body" | "body", Value::String(b)) if b.contains("{{") => Value::from(templated_body(b)),
                        ("raw_body" | "body", Value::String(b)) => match serde_json::from_str::<Value>(b) {
                            Ok(j) => Value::from(json(&j).to_string()),
                            Err(_) => Value::from(text_value(b)),
                        },
                        // Code the user wrote (a visualization): no API data in it.
                        ("visualizer", _) => child.clone(),
                        (m, Value::Object(map)) if VALUE_MAPS.contains(&m) => Value::Object(
                            map.iter()
                                .map(|(name, val)| {
                                    (
                                        name.clone(),
                                        match val {
                                            Value::String(s) => {
                                                // An input called `login`: hidden in the chat too.
                                                if personal_key(name) && !s.contains("{{") && !s.trim().is_empty() {
                                                    remember(&fingerprint(s), s, true);
                                                }
                                                Value::from(text_value(s))
                                            }
                                            other => json(other),
                                        },
                                    )
                                })
                                .collect(),
                        ),
                        _ => walk(child, Some(k), vocabulary),
                    };
                    out.insert(k.clone(), anon);
                }
                Value::Object(out)
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| walk(x, key, vocabulary)).collect()),
            // The user's own words (a name, a note): the logins / names / emails in them are hidden.
            Value::String(s) if key.is_some_and(|k| FREE_TEXT.contains(&k)) => Value::from(mask_text(s)),
            Value::String(s) if key.is_some_and(|k| STRUCTURE_KEYS.contains(&k)) => Value::String(s.clone()),
            Value::String(s) => Value::from(text_value(s)),
            Value::Number(_) if key.is_some_and(|k| STRUCTURE_KEYS.contains(&k) || k.ends_with("_ms") || k == "max_pages" || k == "ms") => v.clone(),
            other @ (Value::Number(_) | Value::Bool(_) | Value::Null) => match other {
                Value::Number(_) => scalar(other),
                _ => other.clone(),
            },
        }
    }
    walk(v, None, vocabulary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_value_gets_through() {
        let data = json!({
            "id": 917, "login": "jdoe", "email": "jdoe@example.com", "staff?": false, "location": null,
            "created_at": "2024-09-01T08:00:00.000Z", "image": { "link": "https://cdn.example.com/users/jdoe.jpg" },
            "orders": [ { "total": 4.2, "store": { "id": 21, "slug": "downtown" } } ],
            "list": [1, 2, 3, 4, 5, 6, 7]
        });
        let anon = json(&data);
        let text = anon.to_string();
        for value in ["917\"", "jdoe", "example.com", "4.2", "downtown", "2024-09", "cdn.example"] {
            assert!(!text.contains(value), "{value} leaked: {text}");
        }
        assert!(anon["login"].as_str().unwrap().starts_with("string#"));
        assert!(anon["email"].as_str().unwrap().starts_with("string (email)#"));
        assert!(anon["created_at"].as_str().unwrap().starts_with("string (date-time)#"));
        assert!(anon["image"]["link"].as_str().unwrap().starts_with("string (url)#"));
        assert_eq!(anon["staff?"], "boolean");
        assert_eq!(anon["location"], Value::Null);
        assert!(anon["orders"][0]["store"]["id"].as_str().unwrap().starts_with("integer#"));
        assert_eq!(anon["list"].as_array().unwrap().len(), LIST_ITEMS + 1);
        assert_eq!(anon["list"][LIST_ITEMS], "… 2 more items");
        // The same value, the same fingerprint (links stay visible); another, another.
        assert_eq!(json(&json!(17)), json(&json!({"x": 17}))["x"]);
        // Keys that are data (dates, logins in a table) are values too.
        let by_day = json(&json!({"2017-11-13": 4, "2017-11-14": 1}));
        assert!(!by_day.to_string().contains("2017"), "{by_day}");
        assert!(by_day.as_object().unwrap().keys().all(|k| k.starts_with("{date}#")));
        let table: serde_json::Map<String, Value> = (0..20).map(|i| (format!("login{i}"), json!(i))).collect();
        assert!(!json(&Value::Object(table)).to_string().contains("login1"));
        assert!(json(&json!({"id": 1, "login": "x"})).get("login").is_some(), "an ordinary object keeps its keys");
        assert_ne!(json(&json!(17)), json(&json!(18)));
    }

    #[test]
    fn saved_requests_and_flows_keep_their_structure() {
        let vocab: HashSet<String> = ["v1", "users", "orders"].iter().map(|s| s.to_string()).collect();
        assert_eq!(path("/v1/users/:id/orders", &vocab), "/v1/users/:id/orders");
        let p = path("/v1/users/jdoe/orders?filter[store_id]=12&page[size]={{n}}", &vocab);
        assert!(p.starts_with("/v1/users/<string#") && !p.contains("jdoe") && !p.contains("=12"), "{p}");
        assert!(p.ends_with("page[size]={{n}}"), "templates stay: {p}");

        let spec = json!({ "name": "Mine", "spec": { "verb": "GET", "path": "/v1/users/:id", "path_values": { "id": "jdoe", "name": "bob" },
            "query": [ { "key": "filter[store_id]", "value": "12" }, { "key": "range[created_at]", "value": "{{now-7d}},{{now}}" } ],
            "raw_body": "{\"user\": {\"login\": \"asmith\"}}", "description": "My notes" } });
        let anon = saved(&spec, &vocab);
        let text = anon.to_string();
        assert!(!text.contains("jdoe") && !text.contains("bob") && !text.contains("asmith") && !text.contains("\"12\""), "{text}");
        assert_eq!(anon["spec"]["query"][0]["key"], "filter[store_id]", "keys are structure");
        assert_eq!(anon["spec"]["query"][1]["value"], "{{now-7d}},{{now}}", "templates are how it works");
        assert_eq!(anon["name"], "Mine");

        let flow = json!({ "name": "F", "inputs": [ { "type": "string", "name": "login", "value": "jdoe" } ],
            "blocks": [ { "type": "if", "left": "{{p.status}}", "op": "eq", "right": "finished" }, { "type": "wait", "ms": 500 } ] });
        let anon = saved(&flow, &vocab);
        assert!(!anon.to_string().contains("jdoe") && !anon.to_string().contains("finished"), "{anon}");
        assert_eq!(anon["blocks"][0]["left"], "{{p.status}}");
        assert_eq!(anon["blocks"][1]["ms"], 500, "settings stay");
    }

    #[test]
    fn local_keys() {
        let (a, b) = (random_key(), random_key());
        assert_ne!(a, b);
        assert_eq!(decode_hex(&hex(&a)), Some(a));
        assert_eq!(decode_hex("nothex"), None);
        assert_eq!(fingerprint_u64("x"), fingerprint_u64("x"));
    }

    #[test]
    fn quoted_values_in_messages() {
        let m = message("substitution “Users”: nothing found for “jdoe”");
        assert!(m.starts_with("substitution “string#") && !m.contains("jdoe"), "{m}");
        assert_eq!(message("no quotes"), "no quotes");
    }

    #[test]
    fn literals_next_to_templates() {
        let vocab: HashSet<String> = ["v1", "users"].into_iter().map(String::from).collect();
        for (v, kept) in [("jdoe,{{x}}", ",{{x}}"), ("2026-01-01T00:00:00Z,{{now}}", ",{{now}}"), ("Done for jdoe: {{n}}", ": {{n}}"), ("{{a}} and {{b", "{{a}}")] {
            let out = text_value(v);
            assert!(!out.contains("jdoe") && !out.contains("2026") && !out.contains("Done") && !out.contains("and"), "{v} → {out}");
            assert!(out.contains(kept), "{v} → {out}");
        }
        assert_eq!(text_value("{{now-7d}},{{now}}"), "{{now-7d}},{{now}}");
        let p = path("/v1/users/jdoe{{x}}/x", &vocab);
        assert!(!p.contains("jdoe") && p.contains("{{x}}"), "{p}");

        let flow = serde_json::json!({ "blocks": [ { "type": "request", "body": "{\"user\": {\"login\": \"jdoe\", \"kind\": \"{{kind}}\", \"tag\": \"x-{{k}}\", \"n\": {{n}}, \"ids\": [12, {{id}}]}}" } ] });
        let body = saved(&flow, &vocab)["blocks"][0]["body"].as_str().unwrap().to_owned();
        assert!(!body.contains("jdoe") && !body.contains("12") && !body.contains("x-"), "{body}");
        for kept in ["\"login\"", "\"{{kind}}\"", "{{k}}", ":{{n}}", "{{id}}]"] {
            assert!(body.contains(kept), "{kept} missing: {body}");
        }
        // Not JSON even with the templates: text.
        let flow = serde_json::json!({ "blocks": [ { "type": "request", "body": "{\"login\": \"jdoe\", {{x}}" } ] });
        assert!(!saved(&flow, &vocab).to_string().contains("jdoe"));
    }

    #[test]
    fn small_dictionaries_keyed_by_logins() {
        let anon = json(&serde_json::json!({ "jean-luc": 3, "jdoe": 5 })).to_string();
        assert!(!anon.contains("jean-luc") && !anon.contains("jdoe"), "{anon}");
        let anon = json(&serde_json::json!({ "jean-luc": { "level": 2 }, "jdoe": { "level": 4 } })).to_string();
        assert!(!anon.contains("jdoe") && anon.contains("level"), "{anon}");
        // Records and headers keep their names.
        let user = json(&serde_json::json!({ "id": 1, "signup_year": 2024, "staff?": 0 })).to_string();
        assert!(user.contains("signup_year") && user.contains("staff?"), "{user}");
        let headers = json(&serde_json::json!({ "x-total": "120", "content-type": "application/json" })).to_string();
        assert!(headers.contains("x-total"), "{headers}");
    }

    #[test]
    fn what_the_ai_sends_back_gets_its_values() {
        let anon = json(&serde_json::json!([{ "id": 98765, "login": "zkowalsk", "email": "zk@shop.example" }]));
        let (id, login, email) = (anon[0]["id"].as_str().unwrap(), anon[0]["login"].as_str().unwrap(), anon[0]["email"].as_str().unwrap());
        let input = serde_json::json!({
            "path": format!("/v1/users/<{login}>/orders"),
            "query": [ { "key": "filter[user_id]", "value": id } ],
            "body": format!("{{\"to\": \"{email}\"}}"),
            "other": "issue #1234567890ab stays"
        });
        let back = reveal(&input);
        assert_eq!(back["path"], "/v1/users/zkowalsk/orders");
        assert_eq!(back["query"][0]["value"], "98765");
        assert_eq!(back["body"], "{\"to\": \"zk@shop.example\"}");
        assert_eq!(back["other"], "issue #1234567890ab stays");
        assert_eq!(reveal_text("string#0000000000"), "string#0000000000", "unknown: left alone");

        // Seen once: hidden in what the user writes, and usable again.
        let typed = "Projets de zkowalsk, mail zk@shop.example. Merci";
        let masked = mask_text(typed);
        assert!(!masked.contains("zkowalsk") && !masked.contains("shop.example"), "{masked}");
        assert!(masked.starts_with("Projets de string#") && masked.ends_with(". Merci"), "{masked}");
        assert_eq!(reveal_text(&masked), typed);
        // And as a key.
        let by_login = json(&serde_json::json!({ "zkowalsk": 3 })).to_string();
        assert!(!by_login.contains("zkowalsk"), "{by_login}");
        // A saved item's name and note.
        let flow = serde_json::json!({ "name": "Projects of zkowalsk", "blocks": [ { "type": "log", "note": "for zkowalsk", "message": "x" } ] });
        let anon = saved(&flow, &HashSet::new()).to_string();
        assert!(!anon.contains("zkowalsk") && anon.contains("Projects of"), "{anon}");
    }

    #[test]
    fn bodies_that_are_not_json() {
        assert!(body("Hello jdoe").starts_with("(not JSON: 10 characters"));
        assert!(!body("Hello jdoe").contains("jdoe"));
        assert_eq!(body("  "), "(empty body)");
    }
}
