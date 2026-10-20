//! The native app's own parts. The first time it starts it asks where the
//! board is (`shell`): on a server, signed in through the browser
//! (`oauth`), or a local board, served in-process from a SQLite file
//! (`local`). The answer is remembered (`config`).

pub mod config;
pub mod local;
pub mod oauth;
pub mod shell;
