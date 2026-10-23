//! A local board: its4tasks-server running inside the app, on a loopback
//! port, over `board.sqlite` in the data directory (`~/.local/share/its4tasks`
//! on Linux). Only the app can use it: it trusts a secret made at start.

use std::sync::mpsc;

use its4tasks_server::api::Api;
use its4tasks_server::auth::Auth;
use tokio::sync::oneshot;
use uuid::Uuid;

/// The running local server. Dropping it stops it.
pub struct LocalServer {
    pub url: String,
    pub secret: String,
    _stop: oneshot::Sender<()>,
}

/// Where the local board is kept.
fn database_url() -> Result<String, String> {
    let dirs = directories::ProjectDirs::from("", "", "its4tasks")
        .ok_or("there's no data directory to keep the board in")?;
    let file = dirs.data_dir().join("board.sqlite");
    Ok(format!("sqlite://{}?mode=rwc", file.display()))
}

/// Opens the local board and starts serving it.
pub fn start() -> Result<LocalServer, String> {
    start_at(database_url()?)
}

/// Opens the board in the database at `url` and starts serving it.
pub fn start_at(url: String) -> Result<LocalServer, String> {
    let secret = Uuid::new_v4().simple().to_string();
    let (stop, stopped) = oneshot::channel::<()>();
    let (ready, started) = mpsc::channel();
    let auth = Auth::Local {
        secret: secret.clone(),
    };
    std::thread::Builder::new()
        .name("local-server".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => return drop(ready.send(Err(e.to_string()))),
            };
            rt.block_on(async move {
                let api = match Api::connect(&url).await {
                    Ok(api) => api,
                    Err(e) => return drop(ready.send(Err(format!("opening the board: {e}")))),
                };
                let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                    Ok(l) => l,
                    Err(e) => return drop(ready.send(Err(e.to_string()))),
                };
                let addr = listener.local_addr().map_err(|e| e.to_string());
                let _ = ready.send(addr);
                let stop = async {
                    let _ = stopped.await;
                };
                if let Err(e) =
                    its4tasks_server::server::serve_until(listener, api, auth, stop).await
                {
                    eprintln!("its4tasks: the local board stopped: {e}");
                }
            });
        })
        .map_err(|e| e.to_string())?;
    let addr = started
        .recv()
        .map_err(|_| "the local board didn't start".to_owned())??;
    Ok(LocalServer {
        url: format!("http://{addr}"),
        secret,
        _stop: stop,
    })
}
