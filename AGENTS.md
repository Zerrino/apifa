# apifa — guide for agents and editors

apifa (API For All) is a desktop API client: HTTP, GraphQL, WebSocket and gRPC requests, collections in folders, environments, authentication, pre / post scripts, flows that chain calls, tests. Saved requests and flows are plain JSON files. Edit them with any editor or AI agent: the app reloads them live.

- Workspace folder: `~/.config/apifa/workspace/` (print it with `apifa path`)
- Each folder is a real folder. Each saved request is a `*.request.json` file; each flow is a `*.flow.json` file.
- `uid` fields are added by the app when missing. Don't copy a `uid` into a new file: leave it out.
- `.folder.json` files belong to the app. Leave them alone.

## Command line (to test what you edit)

```sh
apifa check                         # validate every file of the workspace
apifa check path/to/file.flow.json  # validate one file
apifa list                          # saved requests and flows
apifa run "Flow name" -i username=jdoe # run a flow (name, file path or uid), print its console
apifa run my.flow.json --vars       # …and print the final variables as JSON
apifa run "Flow name" --resume      # go on with a run that was stopped / failed / killed
apifa test                          # send every saved request, run every flow, check their assertions
apifa test "Folder" --junit out.xml # …only a folder (or a request / flow name or file), JUnit report
apifa mock [--port 7643]            # mock server: a local stand-in for your API
apifa run "Flow" --mock             # run / test against a mock server (no network, no credentials)
apifa run "Flow" --env prod         # with an environment's {{values}} (default: the active one)
apifa docs [folder] -f html|md|openapi -o out   # documentation of saved requests and flows
apifa docs --api [users pets] -f html -o out   # documentation of the imported APIs (Endpoints tab › Generate docs)
apifa endpoints users orders        # search the imported API docs (shows required / optional params)
apifa import-api openapi.yaml       # import an API definition (OpenAPI 3, Swagger 2, apipie; a file or a URL)
apifa apis [--remove name]          # the imported API definitions
apifa profiles                      # the API profiles (any command takes --profile name)
apifa understand [--mock]           # map the structure of the data from every GET (DATA_MAP.md); --saved-only
apifa datamap -o map.html           # export the data map (.json / .md / .html; none = Markdown printed)
apifa datamap --load map.json       # make an exported map the current one
apifa path                          # the workspace folder
```

If the user turned on **Settings › MCP server**, an MCP client can also drive the open app (http://127.0.0.1:7642/mcp, `Authorization: Bearer <key>` shown in the settings): search the docs, read the workspace, open filled-in requests / flows / substitutions in tabs, and send requests (the user confirms each one). What it returns is anonymized (each value is its type and a fingerprint, `string#5d1e0a9b3c`); to use a value, write its fingerprint as is in a tool input: the app puts the real value back before using it.

`run` and `test` use the base URL and the authentication of the app settings (and of the requests' folders). `run` exits with status 1 when the flow fails.

Stopping and resuming: every successful response of a run is written as it arrives to `runs/<flow uid>.resume.jsonl` (data folder). A run that stops before the end (Stop button, error, Ctrl+C, app closed, hourly limit) can be resumed (app: **Resume**; CLI: `--resume`): the flow runs again from the top with the inputs of the stopped run, the responses already received are reused instead of sent, and waits already passed are skipped, so it picks up where it stopped. A finished run deletes the file; **Run** / **Start over** begins a new one. Failed responses (4xx/5xx) aren't kept: they are asked again. Editing the flow's blocks in between may stop the replay from matching.

## Saved request — `Name.request.json`

```json
{
  "name": "Orders of a user",
  "spec": {
    "verb": "GET",
    "path": "/users/:user_id/orders",
    "path_values": { "user_id": "7" },
    "query": [ { "key": "limit", "value": "100" } ],
    "headers": [ { "key": "Accept-Language", "value": "fr" } ],
    "auth": { "type": "bearer", "token": "{{token}}" },
    "post_script": "pm.test(\"200\", () => pm.response.to.have.status(200));",
    "body_mode": "Raw",
    "raw_body": ""
  }
}
```

- `verb`: `GET` `POST` `PUT` `PATCH` `DELETE`.
- `path` is relative to the base URL — the active environment's `baseUrl`, else Settings › Base URL — or a full URL (`https://…`). `:name` segments are filled from `path_values`.
- `body_mode`: `"Raw"` sends `raw_body` as JSON; `"Text"` sends it as `content_type` (XML, plain text, a form…); `"GraphQl"`: `raw_body` is the operation, `graphql_variables` its variables (JSON); `"Form"` builds JSON from the documented fields (`values`).
- File uploads: `"files": [ { "key": "user[image]", "value": "~/Pictures/me.jpg" } ]` (Raw mode), or the path as the documented field's form value (`"values": {"user[image]": "~/Pictures/me.jpg"}`). With a file, the request goes out as `multipart/form-data` and the JSON body becomes text fields (`user[login]=…`).
- Optional `doc` (`{"resource": "users", "method": "index", "route": 0}`) links the request to its documentation. Form `values` (`{"page[size]": "5"}`) are only used with `doc`.
- `{{now}}`, `{{now-7d}}`, `{{today}}` are replaced by dates when the request is sent.

- Optional `description`: your notes about the request (Docs tab), shown in generated documentation (folder menu › Generate documentation, `apifa docs`: HTML page, Markdown or OpenAPI 3 with the API docs of each endpoint, parameters and their saved values, cURL, assertions, an example response; `--no-examples` leaves the responses out). A flow's `description`, inputs and outputs are used the same way.
- Optional `assertions`: checked on every response (request tab › Tests, folder menu › Run tests, `apifa test`), like Bruno's Assert tab:

  ```json
  "assertions": [
    { "expr": "res.status", "op": "eq", "value": "200" },
    { "expr": "res.body", "op": "isNotEmpty" },
    { "expr": "res.body[0].email", "op": "matches", "value": "@" },
    { "expr": "res.headers.x-total", "op": "gt", "value": "100" }
  ]
  ```

  `expr` is a path in `res` = `{status, headers, body, time, size}` (`time` in ms, header names lowercase, `res.` optional; `.length`, `[-1]` and the other template paths work). `op`: `eq` `neq` `gt` `gte` `lt` `lte` (numbers, or dates as dates) `in` `notIn` (`a,b,c`) `contains` `notContains` `length` `matches` `notMatches` (regex, `/…/i` works) `startsWith` `endsWith` `between` (`min,max`) and, without `value`: `isEmpty` `isNotEmpty` `isNull` `isUndefined` `isDefined` `isTruthy` `isFalsy` `isJson` `isNumber` `isString` `isBoolean` `isArray`. `"enabled": false` skips one. A request with assertions passes when they all hold (a 404 can be expected); one without passes on a status below 400.
- Optional `mock`: what the mock server answers for this request — `{ "status": 200, "body": "[{\"id\": 1}]", "headers": [], "delay_ms": 0, "enabled": true }` (`body` is text: JSON or not). It answers requests with the same verb and a path that fits: a `path_values` entry that is filled must be equal (`/users/:id` with `id` = `7` answers only `/users/7`; empty = any id), and the request's own query parameters (`page[…]` aside) must be there with the same values; the most specific mock wins. The body can use `{{path.id}}`, `{{query.name}}`, `{{now}}`. Lists are paginated like the API (`page[size]`, `page[number]`, `X-Total`). Without a mock, the server answers the docs' example, then the example recorded from a real call, else a 404; the `X-Mock-Source` header says which. `POST /oauth/token` gives a fake token. The app runs it from Settings › Mock server (port 7643, 127.0.0.1 only), and can send all its requests there.
- Optional `visualizer`: shows the response in the browser (Visualize button). Either HTML + JavaScript using `response` (`{status, headers, body}`) and `body`, or a Postman script: JavaScript with `pm.response.json()` / `.text()` / `.code` / `.headers.get(name)` and `pm.visualizer.set(template, data)`, the template being Handlebars (`{{x}}`, `{{{html}}}`, `{{#each}}`, `{{#if}}`, `{{else}}`, `{{@index}}`, `../`; the real Handlebars when online, a built-in subset offline). A script may declare its own `const response`. The page can't send the data anywhere (Content-Security-Policy: no `fetch` / XHR, images only from the API the response came from, scripts and styles only from cdnjs / jsdelivr / unpkg).

## Authentication

A request's `auth` (Auth tab) is `Inherit` when missing: it takes its folder's (Folder settings…, kept in `.folder.json`), else the workspace's (Settings › Authentication; its secrets stay in the system keychain). Every field is a template: keep secrets in environment variables (`{{token}}`, marked secret).

```json
{ "type": "none" }
{ "type": "basic", "username": "{{user}}", "password": "{{password}}" }
{ "type": "bearer", "token": "{{token}}" }
{ "type": "api_key", "key": "X-API-Key", "value": "{{api_key}}", "location": "header" }
{ "type": "oauth2", "grant": "client_credentials", "token_url": "https://auth.example.com/oauth/token",
  "client_id": "{{client_id}}", "client_secret": "{{client_secret}}", "scope": "read write" }
```

OAuth 2.0 grants: `client_credentials`, `password` (`username`, `password`), `authorization_code` (`auth_url`; the browser opens, PKCE, redirect to `http://127.0.0.1:<redirect_port>/callback`). Tokens are cached per configuration and refreshed (refresh token, or again after a 401). `client_auth`: `body` (default) or `header` (HTTP Basic). A header the request sets itself (`Authorization`) wins.

## Scripts — `pre_script`, `post_script`

JavaScript (QuickJS; no network, no files) run before sending and on the response, with Postman's `pm` API:

- `pm.environment` / `pm.variables` / `pm.globals`: `get`, `set`, `unset`, `has`, `replaceIn("{{x}}")`. `pm.environment.set` is saved into the active environment.
- `pm.request`: `url`, `method`, `headers` (`get`, `add`, `upsert`, `remove`), `body.raw` — a pre-request script can change them. `pm.execution.skipRequest()`.
- `pm.response`: `code`, `status`, `responseTime`, `headers.get(name)`, `json()`, `text()`, `to.have.status(200)`.
- `pm.test("name", () => pm.expect(x).to.equal(1))` (chai-like: `equal`, `eql`, `include`, `property`, `lengthOf`, `above`, `below`, `match`, `a("string")`, `.not`…): results join the assertions (Tests tab, test runner); `console.log` goes to the response's Console tab.

## GraphQL, WebSocket, gRPC

- **GraphQL**: `"body_mode": "GraphQl"`, `raw_body` = the operation, `graphql_variables` = JSON. The Body tab fetches the schema (introspection) and writes operations from it.
- **WebSocket**: `"protocol": "web_socket"`, `path` = `wss://…` (or a path: the base URL in ws:// / wss://), `headers`, `auth`, `ws_subprotocols`, `ws_messages` (messages kept to send).
- **gRPC**: `"protocol": "grpc"`, `path` = `https://host:port` (`http://` for plaintext), `grpc_method` = `/package.Service/Method`, `proto_files` / `proto_includes` (empty: server reflection), `raw_body` = the message as JSON (client / bidi streaming: a JSON array), `headers` = metadata.

## API profiles — `profiles.json` (next to `workspace/`)

One profile per API you work with (like a Postman / Bruno collection): its own base URL, its own authentication (so its own tokens), the API definitions that describe it, its own data map (`datamap.<id>.json`, `DATA_MAP.<id>.md`). Importing an API definition makes its profile. A folder bound to a profile (Folder settings…, `"profile": "<id>"` in `.folder.json`) makes its requests and flows use it; a request can name one too (`"profile": "<id>"` in its spec); the others use the active profile (title bar, `--profile name` on the command line, `apifa profiles` lists them).

```json
{ "active": "pets", "list": [
    { "id": "pets", "name": "Pets", "base_url": "https://pets.example.com/v1", "apis": ["Pets"],
      "auth": { "type": "api_key", "key": "X-API-Key", "value": "{{pets_key}}" } },
    { "id": "shop", "name": "Shop", "base_url": "{{shopUrl}}", "apis": ["Shop"],
      "auth": { "type": "oauth2", "grant": "client_credentials", "token_url": "https://auth.shop.example/token", "client_id": "{{shop_id}}", "client_secret": "{{shop_secret}}" } } ] }
```

Which auth a request uses: its own, else its folders', else its profile's, else the workspace's (Settings). Which base URL: a full URL in the request, else its profile's, else the environment's `baseUrl`, else Settings › Base URL. Understand maps the active profile: its saved GET requests (and those bound to no profile) and the GETs of its API definitions.

## API definitions — `apis/` (next to `workspace/`)

Imported OpenAPI 3 / Swagger 2 (JSON or YAML) or apipie definitions (sidebar › Endpoints › APIs: a file or a URL; `apifa import-api`). Their endpoints make the sidebar, the request forms (documented fields), the examples, Understand and the generated docs. Each import makes the API's profile (its first server as base URL, the kind of auth it declares, to fill in), active when none is; the sidebar groups the endpoints by API.

## Substitutions — `substitutions.json` (next to `workspace/`)

Type a name (a login, a slug…) where an id is expected: the app looks the id up just before sending (request tabs and flows: path `:variables`, documented fields, query parameters, raw JSON body).

```json
[ { "kind": "folder", "name": "Ids from names", "items": [
    { "kind": "rule", "name": "User id from username", "fields": ["user_id"], "when": "not_number",
      "path": "/users", "query": [ { "key": "username", "value": "{{value}}" } ], "extract": "body[0].id" }
] } ]
```

- `fields`: `user_id` also matches `:user_id`, `filter[user_id]`, `user[user_id]`. Comma lists (`jdoe,asmith`) are replaced item by item.
- `when`: `not_number` (default: `12345` is sent as is) or `always`. Values with `{{…}}` are left alone.
- `{{value}}` is what was typed; `extract` is a path in `{status, body}`. Nothing found = the request fails with a clear message.

## Data map — `DATA_MAP.md` (in the workspace folder)

**Understand** (Workspace › Data map › Understand, or `apifa understand`) sends the saved GET requests, then every documented GET of the imported APIs — one that needs an id (`:user_id`, a required `filter[account_id]`…) is sent with an id an earlier GET returned, round after round until nothing more unlocks (only GETs, at the configured pace; a 404 is tried with another id) — and maps the structure of what comes back: the entities (`users`, `orders`…), their fields (type, format, how often present, nullable, closed sets) and how they link — an object embedded in another (`orders[].user`), a list of them, or an id (`user_id`); a link is *confirmed* when its ids are ids the target's own endpoints returned. No value is kept (the ids used to unlock endpoints stay in memory during the run). Each entity is a tree of its fields; a field that links to another entity opens on that entity's fields. Read `DATA_MAP.md` to know where a piece of information lives before writing a flow (the assistant and MCP clients: the `data_map` tool). The app shows it in the Data map tab (it reopens on the saved map, nothing is sent again) as a tree with a search, and as layers of dependency (Layers: only the entities a GET returned; layer 0 is what nothing else points to, each entity sits above what it uses) and the generated documentation includes it. A GET sent from a request tab on an endpoint the map hasn't read yet is added to it, and Understand saves an example response for every GET the docs give none for. The links of the request forms (`user_id` → GET /users) and the generated documentation (what each endpoint returns, where each parameter's values come from, the structure of the API) follow the map. **Explained by AI** (request page › Explain with AI, with an AI key and a map): what an endpoint does, written from its docs, an example of its GET's answer and the map (all anonymized), kept in `ai_explanations.json`; `endpoint_details` gives it to the assistant. **Export** (Data map tab, or `apifa datamap`) saves it as JSON (loaded again with **Load…** / `--load`, e.g. on another computer), Markdown or an HTML page; none holds a value.

## Environments — `environments.json` (next to `workspace/`)

Named sets of values used as `{{name}}` in every request (path, path variables, fields, query, headers, body) and as starting variables of flows (a flow's inputs replace them). One is active (title bar of the app; `--env name` for `run` / `test`, `--env none` for none).

```json
{ "active": "dev", "list": [
    { "name": "dev", "vars": [ { "key": "baseUrl", "value": "http://localhost:3000" }, { "key": "token", "value": "…" } ], "secrets": ["token"] },
    { "name": "prod", "vars": [ { "key": "baseUrl", "value": "https://api.example.com" }, { "key": "token", "value": "…" } ], "secrets": ["token"] } ] }
```

## Flow — `Name.flow.json`

```json
{
  "name": "Shipped orders of a user",
  "description": "Looks a user up, then lists their orders that shipped",
  "inputs": [ { "type": "string", "name": "username", "value": "jdoe", "description": "A username" } ],
  "blocks": [
    { "type": "request", "save_as": "users", "verb": "GET", "path": "/users", "query": [ { "key": "username", "value": "{{username}}" } ] },
    { "type": "extract", "from": "users.body[0].id", "into": "user_id" },
    { "type": "request", "save_as": "orders", "verb": "GET",
      "path": "/users/:user_id/orders",
      "query": [ { "key": "page[size]", "value": "100" } ], "all_pages": true },
    { "type": "for_each", "list": "orders.body", "item": "o", "blocks": [
      { "type": "if", "left": "{{o.status}}", "op": "eq", "right": "shipped",
        "then_blocks": [ { "type": "log", "message": "{{o.id}}: {{o.total}}" } ],
        "else_blocks": [] }
    ] }
  ]
}
```

Inputs have a `type`: `string` `integer` `number` `boolean` `date` (also `{{now-7d}}`) `file` (local path, `~/` allowed) `enum` `json`, or `auto` (the default when `type` is missing: `17` becomes a number, `[1,2]` a list, anything else text). An `enum` lists its choices in `options`: `{ "type": "enum", "name": "region", "value": "eu", "options": ["eu", "us"] }`. A value that doesn't match its type stops the run before the first block (`check` reports it too).

A flow can expose `outputs` at the end: `"outputs": [ { "name": "user_id", "value": "{{user.body.id}}", "description": "…" } ]`. `value` is an expression: `{{x}}` keeps the type, text with `{{…}}` inside gives text. Another flow gets them with a `flow` block: `{ "type": "flow", "flow": "Find user", "inputs": { "login": "jdoe" }, "save_as": "found" }`, then `{{found.user_id}}`. A flow can't run itself (even through another flow). `flow_uid` is added by the app when you pick the flow; leave it out in files you write.

A flow can also have a `visualizer` (HTML + JavaScript, like a request's), opened in the browser after a run with Visualize. It gets `flow` (`{name, ok, outputs, vars}`), `outputs`, `vars`, and `body`: the only output, else the outputs object, else every variable — so a script written for a request's `body` works on a flow that outputs a list.

With `"keep_results": true`, each run (app or `apifa run`) writes its variables to `runs/<flow uid>.json` in the app's data folder (not the workspace); the app shows them when the flow is opened, so the visualization can be changed and reopened without running again. Results can also be exported / loaded as JSON (`run … --vars` output loads too).

Blocks run from top to bottom. Every block also accepts `"enabled": false` (skip it), `"note": "…"` (a comment), and `"log": true|false` (show it in the console; `if` blocks are quiet by default).

| `type` | Fields | What it does |
|---|---|---|
| `request` | `save_as`, `verb`, `path`, `query` [`{key,value}`], `headers` [`{key,value}`], `body` (JSON text), `files` [`{key,value}`] (field name → local path, POST/PUT/PATCH), `values` (`{"user[kind]": "{{kind}}"}`: documented fields → query for GET/DELETE, body fields or files otherwise), `all_pages` (GET), `max_pages` (with `all_pages`: page limit of this block, `0` = none; default: Settings), `stop_on_error` (default true), `doc`, `assertions` (like a saved request's; values can use `{{variables}}`; with assertions they decide — a failed one stops the flow when `stop_on_error`, else it's a warning), `auth` (like a saved request's), `profile` (the API profile it uses; empty: the flow's), `graphql` + `graphql_variables` (a GraphQL operation instead of `body`), `content_type` (send `body` as text of that type: XML…), `pre_script` / `post_script` (JavaScript with `pm`: `pm.variables` are the flow's variables, `pm.variables.set` makes one, `pm.test` results count like assertions) | Calls the API and stores `{status, ok, headers, body}` in the variable `save_as` |
| `extract` | `from` (path), `into` (name) | Copies a value into a variable |
| `set` | `name`, `value` | Sets a variable. The text is parsed as JSON when possible (numbers, lists…) |
| `for_each` | `list` (path), `item` (name), `blocks`, `parallel` (default false) | Runs `blocks` for each element; `{{index}}` is the position. With `"parallel": true`, several items run at once at the API's per-second limit (Settings › Parallel loops): each item starts from the variables before the loop and can't see what the others set; the last item's variables are kept after it; the console still lists the items in order |
| `if` | `left`, `op`, `right`, `then_blocks`, `else_blocks` | `op`: `eq` `ne` `contains` `gt` `lt` `not_empty` `empty` |
| `wait` | `ms` | Pause |
| `log` | `message` | Writes to the console |
| `fail` | `message` | Stops the flow with an error |
| `flow` | `flow` (name of another flow), `inputs` (`{"login": "{{p.login}}"}`), `save_as`, `profile` (the API profile it runs with; empty: this flow's) | Runs that flow with these inputs (missing ones keep its own values) and stores its outputs `{name: value}` in `save_as` |
| `web_socket` | `save_as`, `url` (`wss://…`, or a path: the profile's base URL as ws:// / wss://), `headers`, `subprotocols`, `messages` (sent in order, templates allowed), `wait_ms` (how long to wait for answers after the last activity, default 2000), `max_messages` (0 = any), `until` (stop once an answer contains this text), `auth`, `profile`, `stop_on_error` | Connects, sends, collects the answers, closes; stores `{connected, status, protocol, sent, received: [answers, JSON parsed], closed}` |
| `grpc` | `save_as`, `url` (`https://host:port`, `http://` for plaintext; empty: the profile's base URL), `method` (`/package.Service/Method`), `proto_files` / `proto_includes` (empty: server reflection), `message` (JSON; client streaming: a JSON array), `metadata`, `timeout_ms`, `auth`, `profile`, `stop_on_error` | Calls the method; stores `{status (0 = OK), status_name, ok, message (the first answer), messages, headers, trailers, time}`; a non-OK status stops the flow when `stop_on_error` |

### Variables and templates

- `{{expr}}` works in every text field. `expr` is a path:
  - `user.body.id`
  - `users.body[0].login` (`[-1]` is the last element)
  - `list.length`
  - `users.body.login` (the field of every element, comma-joined in text)
  - `orders.body.user.id.unique` (without duplicates; `.unique.length` counts them)
- A request block can have an `auth` (like a saved request's; fields can use the flow's variables: `{{login.body.token}}`), else its profile's, else the workspace's.
- **Several APIs in one flow**: each request / WebSocket / gRPC block can name its `profile` — read from one API, write to another, each with its own base URL and token. A `flow` block can run a sub-flow with another profile. The flow itself uses its folder's profile, else the active one.
- In `body`, write `{{x}}` outside quotes to insert the raw JSON value, or `"…{{x}}…"` inside quotes to insert text.
- Path segments like `:user_id` are filled from the variable `user_id`.
- A request block whose verb + path match a documented endpoint (`PATCH /users/{{username}}` ↔ `/users/:id`) gets its `doc` automatically, and `values` fill its documented fields. The JSON `body` wins over `values` for the same key.
- File paths in `files` are templates too: `{ "key": "user[image]", "value": "~/photos/{{p.login}}.jpg" }` uploads a different picture for each login of a `for_each`.
- Inputs are variables too.
- Dates: `{{now}}`, `{{now-30m}}`, `{{now+2h}}`, `{{now-7d}}`, `{{now+1w}}`, `{{today}}`. These are computed when used.

### Conventions flows know

- Lists paginated with `page[size]` and `page[number]` (and an `X-Total` header): use `"all_pages": true` to get everything. Once the first page gives the total (`X-Total`), the other pages are fetched in parallel (Settings › Parallel loops) at the per-second limit the API announces. If the page limit cuts the list, the run shows a warning and the response variable has `"truncated": true` (`total` = how many exist); raise `max_pages` (`0` = no limit).
- Filtering uses query parameters:
  - `filter[field]=a,b`
  - `range[created_at]=2026-01-01T00:00:00Z,{{now}}`
  - `sort=-created_at,id`
- Rate limits: flows space their requests (Settings › Delay between requests). A `for_each` with `"parallel": true` goes as fast as the limit the API announces (`x-secondly-ratelimit-limit`) — use it for loops of independent requests (one GET per login…); a 429 is sent again after a pause. The console shows the loop's progress and the time left.
- Many documented GETs have no example response. The first successful GET to one of them (from the app or the assistant) is saved as its example (`learned_examples.json`, lists cut to 2 items; Examples tab).
- A field you can `sort` or `filter` on isn't always in the response . Look at one real element (`page[size]=1`) before testing a field in an `if`.
- `if` compares dates as dates, numbers as numbers; an empty side (missing field) is never greater or smaller.

Workflow for an agent: edit or create a file → `apifa check` → `apifa run "<name>" -i key=value` → read the output → iterate.
