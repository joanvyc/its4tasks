//! What the server and the GUI share: the board's data and rules (`model`,
//! `log`), the changes clients send (`api`), and reading `board.json` files
//! (`import`). Builds for the browser too.

pub mod api;
pub mod import;
pub mod log;
pub mod model;
