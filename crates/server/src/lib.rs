//! Keeps users' boards (`api`) in a database (`storage`) and serves them
//! over HTTP, along with the web GUI (`server`), to whoever signs in (`auth`,
//! and `clerk` for keeping Clerk sign-ins fresh in the browser).

pub mod api;
pub mod auth;
pub mod clerk;
pub mod server;
pub mod storage;
