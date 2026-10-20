//! The its4tasks GUI (`app`), natively or in the browser, and its link to
//! the server (`client`).

pub mod app;
pub mod client;
pub mod files;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;

pub use its4tasks_core::{api, import, log, model, welcome};
