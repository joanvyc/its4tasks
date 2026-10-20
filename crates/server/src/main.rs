//! Keeps the board in the data directory and serves it over HTTP to the
//! `its4tasks` GUI. Listens on `$ITS4TASKS_ADDR`, by default 127.0.0.1:7878.
//!
//! For testing, `$ITS4TASKS_DELAY_MS` holds every answer back that long, as
//! a slow network would.

use std::process::ExitCode;
use std::time::Duration;

use axum::extract::Request;
use axum::middleware::{self, Next};
use its4tasks_server::api::Api;
use its4tasks_server::server;
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
    let mut router = server::router(api);
    let delay = std::env::var("ITS4TASKS_DELAY_MS")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .map(Duration::from_millis);
    if let Some(delay) = delay {
        eprintln!("its4tasks-server: delaying every answer by {delay:?}");
        router = router.layer(middleware::from_fn(
            move |request: Request, next: Next| async move {
                tokio::time::sleep(delay).await;
                next.run(request).await
            },
        ));
    }
    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("its4tasks-server: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
