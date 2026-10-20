//! Keeps the board (`api`) in SQLite (`storage`) and serves it over HTTP,
//! along with the web GUI (`server`).

pub mod api;
pub mod server;
pub mod storage;
