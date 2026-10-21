//! What the server and the GUI share: the board's data and rules (`model`,
//! `log`), the changes clients send (`api`), boards in `its4tasks.json` files
//! (`import`), and a new user's board (`welcome`). Builds for the browser too.

pub mod api;
pub mod import;
pub mod log;
pub mod model;
pub mod welcome;
