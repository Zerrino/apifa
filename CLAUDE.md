# apifa — working on the code

apifa (API For All): a Rust (egui / eframe 0.37) desktop API client — HTTP, GraphQL, WebSocket, gRPC, environments, auth, scripts, flows. User-facing flow / request file formats: @AGENTS.md

## Commands

```sh
nix-shell --run 'cargo run'      # run from source (NixOS needs the shell for the GUI libraries)
cargo test                       # unit tests — run after every change
cargo test -- --ignored          # + tests hitting the real token endpoint / local install
cargo test --release bench_ -- --ignored --nocapture --test-threads=1   # timings of the hot paths (src/bench.rs)
cargo clippy --all-targets       # no warnings (CI runs it with -D warnings)
./install.sh                     # rebuild + reinstall the desktop app (Nix profile, ~/.local, or ~/Applications/*.app on macOS)
./uninstall.sh [--purge]
```

To try a change against the user's data without touching it, copy the data folder (`apifa path` shows its `workspace/`; `~/.config/apifa` on Linux, `~/Library/Application Support/apifa` on macOS) somewhere and run with `APIFA_DIR=<copy>`. `XDG_CONFIG_HOME` is ignored on macOS.

## Layout (`src/`)

| File | Role |
|---|---|
| `main.rs` | Window setup; `cli::main` handles command-line arguments first |
| `app/mod.rs` | App shell: state (grouped: `McpState`, `MockState`, `AiSetup`, `WorkspaceDisk`, `Nav`), `enum Action`, tabs, `update` |
| `app/actions.rs` | `App::apply` (runs the `Action`s after the frame), sending requests, `App::poll` |
| `app/chrome.rs` · `sidebar.rs` · `dialogs.rs` · `settings.rs` · `services.rs` | Title bar / tab strip / central area · sidebar (endpoints, favorites, workspace, history, substitutions) · dialogs · settings window · mock server, MCP server, AI assistant |
| `ui_request.rs` | Request editor and response viewer |
| `ui_flow.rs` | Flow block editor and run console |
| `ui_date.rs` | Calendar popup for date fields |
| `widgets.rs` | Shared widgets (rows, pills, splitter, painted icons, the apifa logo, background) |
| `flow/mod.rs` | Flow model (blocks, inputs, outputs, serde) |
| `flow/template.rs` | `{{template}}` engine: paths, rendering, comparisons |
| `flow/runner.rs` | Runner thread: requests, pagination, loops (parallel), sub-flows, resume |
| `model.rs` | Request spec → HTTP request (path vars, query, JSON body) |
| `net.rs` | HTTP calls (ureq), with the request's auth |
| `auth.rs` · `ui_auth.rs` | Authentication: None / Basic / Bearer / API key / OAuth 2.0 (client credentials, password, authorization code + PKCE, refresh), `Inherit` from folders / workspace; token cache; the editor |
| `profiles.rs` · `app/profiles_ui.rs` | API profiles: base URL, auth, definitions, data map per API; `profiles::current(id)` resolves a request's (empty = active) |
| `apis.rs` · `openapi.rs` | Imported API definitions (`apis/`): OpenAPI 3 / Swagger 2 (JSON / YAML) and apipie → the `ApiDoc` the app works from |
| `script.rs` | Pre-request / post-response JavaScript (QuickJS via rquickjs, Postman `pm` API), run by `testrun::Sender::send` |
| `graphql.rs` | GraphQL bodies, introspection, schema, operations, lint / format |
| `ws.rs` · `grpc/` · `ui_live.rs` · `app/live.rs` | WebSocket client (tungstenite), dynamic gRPC client (tonic + prost-reflect + protox, reflection), their tabs (a saved request with `protocol`) |
| `docs.rs` | The docs model (resources, methods, routes, params, examples), search, routes matching; apipie parsing (`tests/fixtures/apidoc.json` is the tests' doc) |
| `dates.rs` | `{{now}}`-style dynamic dates, parsing / formatting |
| `workspace.rs` | Folder / request / flow tree, import / export (incl. Postman) |
| `workspace_fs.rs` | Workspace stored as files in `~/.config/apifa/workspace/`, live reload |
| `storage.rs` | config / history / session / favorites JSON files |
| `env.rs` · `app/envs.rs` | Environments (`{{baseUrl}}`, `{{token}}`… in requests and flows), `environments.json`; title-bar picker and editor |
| `privacy.rs` | What AIs (assistant, MCP) get: values replaced by their type + an HMAC fingerprint (local key `anonymization.key`), saved requests' values too (literals next to `{{templates}}` included), keys that are data too. Every tool result goes through it. The fingerprints given are kept in memory (`reveal`: the AI writes `string#…` back in a tool input, the app sends the value) and the logins / names / emails seen are hidden in free text (`mask_text`: chat messages, names, notes) |
| `datamap.rs` · `app/datamap_ui.rs` | Understand: the saved GETs then every documented GET (ids found unlock the ones that need them) → entities, field shapes, links (confirmed by id fingerprints), no value; `datamap.json`, `DATA_MAP.md`, docs section, `data_map` tool (overview / one entity / a field), export (`Export`: JSON reloadable, Markdown, HTML) and load; the Data map tab (`TabBody::DataMap`, one at most): Tree, and Layers (`datamap::layers`: dependency layers, cycles broken, few crossings) |
| `explain.rs` | “Explained by AI” of an endpoint: from its docs, an example of its GET's answer and the data map (anonymized), `ai_explanations.json` |
| `secrets.rs` | SECRET / AI keys / MCP key in the system keychain (`keyring`), `config.json` without them; `APIFA_KEYCHAIN=0` keeps them in the file |
| `subst.rs` | Substitutions (a login typed where an id is expected → looked up before sending), `substitutions.json` |
| `visualize.rs` | Response visualizations (a request's HTML/JS script, opened in the browser) |
| `ai.rs` | AI assistant: providers (Anthropic Messages API, OpenAI-compatible), tools, system prompt |
| `ui_ai.rs` | AI chat panel (right side, hidden without an API key) |
| `mcp.rs` | Optional MCP server (Settings): Streamable HTTP on 127.0.0.1, Bearer key; tools run on the UI thread (`App::mcp_poll`) |
| `runs.rs` | A flow run's results kept in a file (`runs/<uid>.json`, "Keep results", export / load) |
| `learned.rs` | Example responses recorded from real GETs for endpoints the docs give none for (`learned_examples.json`) |
| `install.rs` | Detects and removes the installed app (Settings › Uninstall) |
| `cli.rs` | `run`, `test`, `understand`, `datamap`, `import-api`, `apis`, `mock`, `docs`, `check`, `list`, `endpoints`, `path` |
| `mock.rs` · `docgen.rs` | Mock server (a local stand-in for the API) · generated documentation (HTML, Markdown, OpenAPI; the data map page) |
| `theme.rs` | Colors (graphite chrome, violet accent) and fonts |
| `bench.rs` | Timings of the hot paths (ignored tests) |
| `lib.rs` · `tests/cli.rs` | The crate is a library (`main.rs` only starts the app or the CLI); `tests/cli.rs` runs the built binary against the mock server |

## Conventions

- UI code never mutates app state across borrows: widgets push an `Action`, and `App::apply` runs it after the frame.
- Network calls and flow runs happen on threads that talk back through channels (polled in `App::poll`).
- Colors come from `theme.rs`. Icons are painted (`widgets::paint_icon`), never taken from font glyphs.
- Every on-disk format keeps backward compatibility: new fields need `#[serde(default)]`.
- Hot paths: nothing per frame that walks the disk, re-parses a response or searches the whole doc; cache it and invalidate on change (see `ApiDoc::search`, `log_hits`, `pretty_body`). Measure with `src/bench.rs`.
- Nothing about a particular API is written in the code (hosts, paths, field names, ids): read it from the imported definitions (`docs.rs`) or the data map (`datamap::relations`); plain words of a convention (`slug`, `name`, `login` as a personal-data word, `page[size]` pagination) are fine.
- Nothing of the user's data reaches an AI: a new assistant / MCP tool returns values only through `privacy.rs` (`json`, `body`, `saved`, `path`, `message`).
- UI rows mixing left content with right-aligned controls: draw the right part first (`right_to_left`), then the left part in what's left (clipped), or wrap. `ui_flow::layout_tests` checks the console.
- Before reporting a change as done: `cargo build` with no warnings, `cargo clippy --all-targets`, then `cargo test`.
