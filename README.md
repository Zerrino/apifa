# apifa — API For All

A desktop API client in the spirit of Postman and Bruno, written in Rust: **HTTP, GraphQL, WebSocket and gRPC**, collections in folders, environments, authentication, pre / post-request scripts, flows that chain calls across several APIs, tests, a mock server, generated documentation and an optional AI assistant.

- **One native binary**, no web view, no account, no cloud: everything stays on your computer.
- **Plain files**: saved requests and flows are JSON files in a folder — versioned with git, edited by hand or by an AI agent, reloaded live.
- **A command line** too: run flows and tests in CI, import API definitions, generate docs.

---

## Contents

1. [Install](#install)
2. [Quick start](#quick-start)
3. [Concepts](#concepts)
4. [Requests](#requests) — HTTP, GraphQL, WebSocket, gRPC
5. [Authentication](#authentication)
6. [Environments and API profiles](#environments-and-api-profiles)
7. [Scripts](#scripts)
8. [Flows](#flows)
9. [Tests, mock server, documentation](#tests-mock-server-documentation)
10. [Data map](#data-map)
11. [AI assistant, MCP, privacy](#ai-assistant-mcp-privacy)
12. [Command line](#command-line)
13. [Your data](#your-data)
14. [Development](#development)

---

## Install

You need [Rust](https://rustup.rs) (stable).

```sh
git clone https://github.com/Zerrino/apifa.git && cd apifa
./install.sh                             # installs the app (macOS: ~/Applications/apifa.app; Linux: menu entry + ~/.local/bin/apifa)
cargo run --release                      # or just run it
nix-shell --run 'cargo run --release'    # NixOS: the shell brings the GUI libraries
```

`install.sh` uses your Nix profile on NixOS, `~/.local` on other Linux, `~/Applications` on macOS.
To remove it: ⚙ Settings › **Uninstall…**, or `./uninstall.sh` (`--purge` also deletes your data).

## Quick start

1. **Send something**: `+` › **New HTTP request** (Ctrl+T), type a full URL (`https://jsonplaceholder.typicode.com/users`), **Send** (Ctrl+Enter). The examples in the workspace (sidebar › Workspace › Examples) work right away.
2. **Import your API** (optional, recommended): sidebar › **Endpoints** › **APIs** › *Import file…* or paste the URL of its **OpenAPI 3 / Swagger 2** definition (JSON or YAML; apipie works too). You get every endpoint with a form of its parameters, its docs and examples — and an **API profile** for it.
3. **Say where requests go and how they authenticate**: in the profile (title bar › *API: …* › *Edit API profiles…*), or for everything in ⚙ **Settings** (base URL, authentication). Put secrets in an **environment** (`{{token}}`, marked secret).
4. **Save** (Ctrl+S) in your workspace, add tests and scripts, chain requests in **flows**.

## Concepts

| | |
|---|---|
| **Workspace** | Your saved requests and flows, in folders (real folders on disk). Right-click for rename, move, duplicate, export, *Folder settings…*. |
| **Request** | One call: HTTP (any verb, GraphQL), WebSocket or gRPC. A tab while you edit it, a `*.request.json` file once saved. |
| **Flow** | Blocks run from top to bottom: requests, loops, conditions, variables, sub-flows, WebSocket and gRPC calls. A `*.flow.json` file. |
| **API definition** | An imported OpenAPI / Swagger / apipie document: the endpoints of the sidebar, the request forms, examples, docs. |
| **API profile** | One per API you work with (like a collection): its base URL, its authentication — so its own tokens —, its definitions, its data map. |
| **Environment** | Named values (`dev`, `prod`…) used as `{{name}}` everywhere: `{{baseUrl}}`, `{{token}}`, ids… |

**Which base URL** a relative path (`/users/:id`) gets: a full URL in the request wins, else its **profile**'s, else the environment's `baseUrl`, else ⚙ Settings › Base URL.
**Which authentication**: the request's own, else its **folders'**, else its **profile**'s, else the workspace's (Settings).

## Requests

**HTTP** — verb, URL with `:path` variables, query, headers, auth, body:
- **JSON** (with a validator and *Beautify*), **Text / XML** (any Content-Type), **multipart** with files (*Browse…*), or the **documented form** of an imported endpoint (required fields first, types, choices, a calendar for dates).
- **GraphQL**: query + variables; *Fetch the schema* (introspection) gives an explorer — click a field and the operation is written for you; lint and prettify.
- Dates like `{{now}}`, `{{now-7d}}`, `{{today}}` are computed when sending; `{{name}}` comes from the active environment.
- **Response**: status, time, size, pagination (◀ / ▶ when the API says `X-Total`), headers, **Tests**, script **Console**; big bodies stay smooth; Ctrl+F searches. *Visualize* opens it as a page (your HTML/JS, or a Postman `pm.visualizer` script). *Save as mock* keeps it for the mock server.
- **Find a value**: an underlined `user_id` opens the GET that lists users (from the docs, or what the data map learnt).
- **Substitutions**: type a name where an id is expected (`user_id = jdoe`): the id is looked up just before sending (Substitutions tab; rules made from the docs).

**WebSocket** — pick *WebSocket* next to the URL (or `+` › New WebSocket): connect with headers and auth, send text or binary (hex), ping, a timed log of both directions, saved messages.

**gRPC** — pick *gRPC* (or `+` › New gRPC): services from your `.proto` files or from **server reflection**, method picker (unary and streaming), an example message checked against the schema, metadata and auth, status, headers and trailers.

## Authentication

Per request (Auth tab), per folder (*Folder settings…*), per API profile, or for the whole workspace (⚙ Settings) — *Inherit* takes the next one up.

| Type | |
|---|---|
| No auth | nothing added |
| Basic | `Authorization: Basic …` from a user name and a password |
| Bearer token | `Authorization: Bearer <token>` (the prefix can change) |
| API key | a header (`X-API-Key`…) or a query parameter |
| OAuth 2.0 | **client credentials**, **password**, **authorization code** (the browser opens, PKCE, a local callback); tokens are cached per configuration, refreshed with the refresh token, and asked again after a 401 |

Every field is a template (`{{client_secret}}`). The workspace's secrets go to the system keychain (macOS Keychain, Secret Service, Windows Credential Manager); a header you set yourself (`Authorization`) always wins.

## Environments and API profiles

**Environments** (title bar › *Env*): `dev` and `prod` exist from the start; add variables, mark secrets (masked), duplicate one for another stage. Scripts can change them (`pm.environment.set`), the change is saved.

**API profiles** (title bar › *API*): importing a definition makes its profile (base URL from its servers, the kind of auth it declares, to fill in). Each profile has its own tokens and data map; the sidebar groups the endpoints by API, and opening one binds the request to its profile. Bind a folder to a profile (*Folder settings…*) and its requests and flows always use it, whatever profile is active. On the command line: `apifa profiles`, and `--profile <name>` on any command.

## Scripts

JavaScript (QuickJS, sandboxed: no network, no files) **before sending** and **on the response**, with Postman's `pm` API — the Scripts tab has snippets:

```js
// Before sending
pm.request.headers.upsert({ key: "X-Request-Id", value: pm.variables.replaceIn("{{$guid}}") });

// On the response
pm.test("Status code is 200", () => pm.response.to.have.status(200));
pm.test("Has an id", () => pm.expect(pm.response.json()).to.have.property("id"));
pm.environment.set("token", pm.response.json().access_token);
console.log(pm.response.responseTime);
```

`pm.environment` / `pm.variables` / `pm.globals`, `pm.request` (url, method, headers, `body.raw`), `pm.response` (`code`, `json()`, `text()`, `headers.get`, `responseTime`), `pm.test` + `pm.expect` (chai-like), `pm.execution.skipRequest()`. Test results join the request's assertions.

## Flows

A flow chains calls: `+` › **New flow**, then add blocks (or drag endpoints, saved requests and other flows from the sidebar onto it).

| Block | |
|---|---|
| **API request** | any HTTP request (JSON, text, GraphQL, files, documented form), *Fetch all pages*, assertions, scripts, auth, **its own API profile** |
| **WebSocket** | connect, send messages, collect the answers (for a while, a number of them, or until one says something) |
| **gRPC call** | one method with its message(s) and metadata |
| Extract / Set | variables (`{{user.body.id}}`, `{{users.body.login}}` = every login, `.length`, `.unique`, `[-1]`) |
| For each | a loop, optionally **parallel** at the API's rate limit |
| If / else, Wait, Log, Stop with error | control |
| Run a flow | reuse another flow (inputs → outputs), optionally with another profile |

**Several APIs in one flow**: give each block its profile — read orders from the shop API, create contacts in the CRM API with what came back, each with its own base URL and token.

Flows have typed **inputs** (text, number, date, file, enum, JSON…) and **outputs**; a simple fill-in view and an Edit view; a console with progress, rate limits and time left; **Resume** after a stop or a crash (answers already received aren't sent again); *Keep results* to visualize them later.

## Tests, mock server, documentation

- **Tests**: assertions on responses (`res.status eq 200`, `res.body.length gt 0`… 28 operators, like Bruno's) and `pm.test` in scripts; folder menu › **Run tests**, or `apifa test` (JUnit report for CI).
- **Mock server** (⚙ Settings › Mock server, or `apifa mock`): a local stand-in for your API on `127.0.0.1:7643` — your saved mocks, else the docs' examples, paginated like the API; the app can send all its requests there (a MOCK badge shows).
- **Documentation**: folder menu › *Generate documentation*, or `apifa docs` — a searchable offline HTML page, Markdown, or OpenAPI 3 — with each endpoint's docs, parameters (and where their values come from), cURL, assertions, examples and the data map. The endpoints tab documents the imported APIs themselves.

## Data map

*Workspace › Data map* opens a map of how an API's data is built. **Understand** sends the active profile's saved GET requests, then every documented GET of its definitions — one that needs an id (`:user_id`) is sent with an id an earlier GET returned, until nothing more unlocks. **Only GETs**, in the background, the map growing live.

- **Tree**: each entity (`users`, `orders`…) with its fields (type, format, presence, nullable, closed sets); a field linking to another entity opens on it. **Layers**: what depends on what, at a glance.
- **No value is kept**, only the structure. Export it (JSON, Markdown, HTML); it feeds the generated docs, the form links, `DATA_MAP.md` for agents, and the AI.
- A GET you send from a tab on an endpoint the map hasn't read yet is added to it.

## AI assistant, MCP, privacy

**AI assistant** (optional, your own key: ⚙ Settings › AI assistant — Anthropic, OpenAI, Gemini, Mistral, OpenRouter or any OpenAI-compatible server): ask for a request, a flow, a substitution — it reads the docs, the data map and your workspace and **opens it in a tab for you to check**; it always asks before sending. *Explain with AI* on a request writes what the endpoint does from its docs, an example and the data map.

**MCP server** (⚙ Settings › MCP server): lets agents (Claude Code, Claude Desktop, Codex, Cursor, VS Code, Gemini CLI, Windsurf…) drive the open app with the same tools — local only, protected by a key; the settings give a ready-to-paste setup.

**Privacy: the AIs never see your data.** Everything an AI gets is anonymized on your computer first: each value becomes its type and a fingerprint — `{"login": "jdoe", "id": 7}` → `{"login": "string#5d1e0a9b3c", "id": "integer#a03f…"}`. The structure stays, equal values have equal fingerprints (an HMAC with a key made on your computer, so they can't be guessed back). The AI can still use a value: it writes the fingerprint, and the app puts the real value back locally just before sending. Logins, names and emails the app has seen are also hidden in what you type.

## Command line

```sh
apifa import-api openapi.yaml               # import an API definition (OpenAPI 3, Swagger 2, apipie; a file or a URL)
apifa apis / apifa profiles                 # the imported definitions / the API profiles
apifa endpoints users orders                # search the imported docs
apifa list                                  # saved requests and flows
apifa check [file]                          # validate the workspace files
apifa run "Flow name" -i username=jdoe      # run a flow, print its console (--vars: the variables as JSON; --resume)
apifa test ["Folder"] --junit out.xml       # send every saved request, run every flow, check assertions and scripts
apifa mock [--port 7643]                    # the mock server
apifa docs [folder] -f html|md|openapi -o out   # documentation (--api: of the imported APIs)
apifa understand / apifa datamap -o map.html    # build / export the data map
apifa path                                  # the workspace folder
# --profile <name> on any command (API profile); --env <name> and --mock on run / test / understand
```

`run` and `test` exit with status 1 when something fails — use them in CI.

## Your data

Everything is in `~/.config/apifa/` (`~/Library/Application Support/apifa/` on macOS; `APIFA_DIR` points elsewhere):

| | |
|---|---|
| `workspace/` | your requests (`*.request.json`) and flows (`*.flow.json`) in folders; `DATA_MAP*.md` |
| `apis/` | imported API definitions, as they came |
| `profiles.json`, `environments.json` | API profiles, environments (use `{{variables}}` marked secret for secrets) |
| `config.json` | settings (readable by you only); the workspace auth's secrets, AI keys and MCP key are in the system keychain (`APIFA_KEYCHAIN=0`: in this file) |
| `history.json`, `favorites.json`, `substitutions.json`, `session.json` | history, starred items, substitutions, open tabs |
| `learned_examples.json`, `datamap*.json`, `ai_explanations.json` | examples recorded from real GETs, data maps, AI explanations |
| `anonymization.key` | the fingerprints' key — never leaves your computer |
| `runs/`, `visualize/` | kept flow results, generated pages |

The file formats (for editors and AI agents) are in [AGENTS.md](AGENTS.md); a copy sits in the workspace, so an agent started there knows them.

## Development

```sh
cargo test                        # unit + integration tests (tests/cli.rs runs the binary against the mock server)
cargo clippy --all-targets        # no warnings
cargo fmt
python3 tests/fixtures/make_fixture.py   # regenerates the test API ("Acme Shop", made up)
```

The code layout and conventions are in [CLAUDE.md](CLAUDE.md); the stack (crates, architecture, files) in [stack.md](stack.md).

License: MIT.
