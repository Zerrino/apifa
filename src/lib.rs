//! apifa (API For All) as a library: the desktop app (`app`), the command line (`cli`) and everything
//! they share. `main.rs` only starts one or the other; integration tests (`tests/`) and benchmarks use it.

mod ai;
mod apis;
pub mod app;
mod assert;
mod auth;
#[cfg(test)]
mod bench;
pub mod cli;
mod datamap;
mod dates;
mod docgen;
mod docs;
mod env;
mod explain;
mod favorites;
mod flow;
mod graphql;
mod grpc;
mod install;
mod json_view;
mod learned;
mod mcp;
mod mock;
mod model;
mod net;
#[allow(dead_code)]
mod openapi;
mod privacy;
mod profiles;
mod resume;
mod runs;
#[allow(dead_code)] // pre-request / post-response scripts, not wired into the app yet
mod script;
mod secrets;
pub mod storage;
mod subst;
mod testrun;
mod theme;
mod ui_ai;
mod ui_auth;
mod ui_date;
mod ui_flow;
mod ui_live;
mod ui_request;
mod ui_tests;
mod visualize;
mod widgets;
mod workspace;
mod workspace_fs;
#[allow(dead_code)]
mod ws;
