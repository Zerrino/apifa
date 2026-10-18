# Stack

The whole app is Rust: one native binary, no web view, no server, no account.

## Overview

```
 ┌─────────────────────────────── apifa (one binary) ────────────────────────────────┐
 │  UI: egui (immediate mode) ── drawn by ── eframe → winit (window) + glow (OpenGL) │
 │    ├── requests / flows → worker threads → ureq · tungstenite · tonic → your APIs │
 │    │                       (channels back to the UI)                              │
 │    ├── API docs: imported OpenAPI / Swagger / apipie definitions (apis/)          │
 │    └── storage: JSON files in ~/.config/apifa/ (serde_json)                       │
 └───────────────────────────────────────────────────────────────────────────────────┘
```

## Language & toolchain

| | |
|---|---|
| Language | Rust, edition 2021 (built with rustc 1.96) |
| Build | Cargo; `Cargo.lock` is committed for reproducible builds |
| Packaging | Nix: `default.nix` (installable package + desktop entry), `shell.nix` (dev shell) |
| Install | `install.sh` / `uninstall.sh`: Nix user profile, or `~/.local` without Nix |

## Dependencies

| Crate | Version | Used for |
|---|---|---|
| `eframe` | 0.37 | App framework: window, event loop, OpenGL rendering (features `glow`, `wayland`, `x11`) |
| `egui` | 0.37 | Immediate-mode GUI: every widget, panel and popup; `Scene` for the data map's pan / zoom |
| `ureq` | 3 | Blocking HTTP client (run on worker threads); TLS through `rustls`, so no OpenSSL |
| `serde` / `serde_json` | 1.x | Every file format and API payload; `preserve_order` keeps JSON keys in API order |
| `chrono` | 0.4 | Date picker and `{{now}}` / `{{now-7d}}` dynamic dates (local time ↔ UTC) |
| `rfd` | 0.17 | Native file dialogs for import / export (XDG desktop portal on Linux) |
| `dirs` | 6 | Platform folders (`~/.config`, `~/.local/share`, `~/.local/bin`) |
| `sha2` | 0.10 | HMAC-SHA256 fingerprints of the values given to AIs (`privacy.rs`) |
| `keyring` | 3 | SECRET / AI keys / MCP key in the system keychain |
| `notify` | 8 | Live reload of the workspace files |
| `parking_lot` | 0.12 | Locks shared with worker threads |
| `rquickjs` | 0.14 | Pre / post-request scripts (QuickJS, no I/O) |
| `tungstenite` · `rustls` · `webpki-roots` | 0.30 · 0.23 · 1 | WebSocket tabs (blocking, TLS) |
| `tonic` · `prost` · `prost-reflect` · `protox` · `tokio` | 0.14 · 0.14 · 0.16 · 0.10 · 1 | gRPC: dynamic messages from `.proto` files or server reflection, over HTTP/2 (a private runtime per call) |
| `serde_yaml_ng` | 0.10 | OpenAPI / Swagger in YAML |
| `regex` | 1.11 | Assertions (`matches`) |

Dev only: `proptest` (property tests of the template engine and the request building), `egui_kittest` (UI layout tests).

Under the hood: `winit` 0.30 (windowing, Wayland + X11), `glow` / `glutin` (OpenGL), `rustls` 0.23 (TLS). About 570 crates in total in `Cargo.lock`.

## Architecture

| Layer | Files | Notes |
|---|---|---|
| Entry | `main.rs`, `lib.rs`, `cli.rs` | No arguments: GUI. Otherwise a CLI (`run`, `test`, `understand`, `datamap`, `mock`, `docs`, `check`, `list`, `endpoints`, `path`) |
| UI shell | `app/` (`mod.rs`, `actions.rs`, `chrome.rs`, `sidebar.rs`, `dialogs.rs`, `settings.rs`, `services.rs`, `envs.rs`, `datamap_ui.rs`) | Title bar (custom, no system decorations), sidebar, tabs (requests, flows, the data map), dialogs, settings. Widgets emit `Action`s, which are applied after each frame |
| Editors | `ui_request.rs`, `ui_flow.rs`, `ui_ai.rs`, `ui_date.rs`, `widgets.rs` | Request form generated from the docs, flow block editor, AI chat, calendar, shared widgets |
| Logic | `model.rs`, `flow/` (model, `template.rs`, `runner.rs`), `dates.rs`, `env.rs`, `subst.rs`, `script.rs`, `graphql.rs` | Request building, flow runner + `{{template}}` engine, dynamic dates, environments, substitutions, scripts, GraphQL |
| Network | `net.rs`, `auth.rs`, `ws.rs`, `grpc/`, `mock.rs`, `mcp.rs` | HTTP with Basic / Bearer / API key / OAuth 2.0 (tokens cached, refreshed, retried once after a 401); WebSocket; gRPC; local mock server; local MCP server |
| AI | `ai.rs`, `privacy.rs`, `explain.rs` | Providers and tools; every tool result anonymized (type + HMAC fingerprint), fingerprints written back revealed locally; “Explained by AI” of endpoints |
| Data map | `datamap.rs` | Understand (only GETs, ids unlocking the endpoints that need them), entities / fields / links, dependency layers, export; grows with the GETs sent from tabs; `relations` (field → entity → its GET) drive the form links and the generated docs |
| Data | `apis.rs`, `openapi.rs`, `docs.rs`, `workspace.rs`, `workspace_fs.rs`, `storage.rs`, `secrets.rs`, `learned.rs`, `runs.rs`, `docgen.rs`, `install.rs` | Embedded apidoc, workspace tree + import/export (incl. Postman), files on disk + live reload, settings / history, keychain, recorded examples, kept runs, generated docs, uninstall |
| Look | `theme.rs`, `assets/` | Graphite chrome and a violet accent, Lato font (+ DejaVu Sans fallback), the apifa logo drawn as vectors |

Concurrency is plain threads plus `std::sync::mpsc` channels. There is no async runtime: each request or flow run gets a thread, and the UI polls the channels every frame.

## Data on disk

All data is in `~/.config/apifa/`. Files are written atomically (temporary file, then rename).

| File | Content |
|---|---|
| `config.json` (mode 0600) | Base URL, workspace auth (its secrets in the system keychain), flow settings |
| `apis/` | Imported API definitions, as they came, and `index.json` |
| `workspace/**.request.json`, `workspace/**.flow.json` | Saved requests and flows, one file each, in real folders (format: `AGENTS.md`) |
| `history.json`, `session.json`, `favorites.json` | Last 300 requests, open tabs, starred items |
| `substitutions.json`, `environments.json` | Login → id rules, named sets of `{{values}}` |
| `learned_examples.json`, `runs/` | Example responses recorded from real GETs, kept flow results |
| `datamap.json`, `workspace/DATA_MAP.md` | The data map (structure only, no value) |
| `ai_explanations.json` | “Explained by AI” of endpoints |
| `anonymization.key` (mode 0600) | The key of the fingerprints given to AIs |

## Assets (embedded in the binary)

| File | License |
|---|---|
| `Lato-*.ttf` | SIL Open Font License (`OFL.txt`) |
| `DejaVuSans.ttf`: symbol fallback | Bitstream Vera / public domain (`DejaVu-LICENSE.txt`) |
| `icon.png`, `icons/*.png`: app icon (the apifa logo) | this project |

## Footprint

These numbers were measured on NixOS (GNOME) with the release build.

- Memory: about 118 MB resident (mostly GPU driver libraries and font atlases).
- CPU: about 0 % when idle, about 2.7 % of one core with the animated stars (can be turned off in Settings).
- Size: about 18 MB binary; 21 MB installed.
