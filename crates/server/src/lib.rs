//! Keeps users' boards (`api`) in a database (`storage`) and serves them
//! over HTTP, along with the web GUI (`server`), to whoever signs in (`auth`).

pub mod api;
pub mod auth;
pub mod server;
pub mod storage;
