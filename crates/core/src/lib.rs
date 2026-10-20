//! What the server and the GUI share: the board's data and rules (`model`,
//! `log`) and the changes clients send (`api`). Builds for the browser too.

pub mod api;
pub mod log;
pub mod model;
