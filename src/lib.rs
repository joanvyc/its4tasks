//! Shared by the two binaries: `its4tasks-server`, which keeps the board
//! (`api`, `server`, `storage`), and the `its4tasks` GUI, which reaches it
//! over HTTP (`client`). Both use the board's types and rules (`model`, `log`).

pub mod api;
pub mod client;
pub mod log;
pub mod model;
pub mod server;
pub mod storage;
