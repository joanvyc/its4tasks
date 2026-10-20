//! Keeps the board in the data directory and serves it over HTTP to the
//! `its4tasks` GUI. Listens on `$ITS4TASKS_ADDR`, by default 127.0.0.1:7878.

use std::process::ExitCode;

use its4tasks::api::Api;
use its4tasks::server;
use tokio::net::TcpListener;

const DEFAULT_ADDR: &str = "127.0.0.1:7878";

#[tokio::main]
async fn main() -> ExitCode {
    let addr = std::env::var("ITS4TASKS_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_owned());
    let api = match Api::open().await {
        Ok(api) => api,
        Err(e) => {
            eprintln!("its4tasks-server: failed to open the database: {e}");
            return ExitCode::FAILURE;
        }
    };
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("its4tasks-server: failed to listen on {addr}: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("its4tasks-server: listening on {addr}");
    if let Err(e) = server::serve(listener, api).await {
        eprintln!("its4tasks-server: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
