//! What the native app shows around the board: the first time, a choice of
//! where the board is; then the board, kept signed in.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::App;
use eframe::egui::{self, Button, CentralPanel, RichText, TextEdit, Vec2};

use super::config::{Config, Source};
use super::local::{self, LocalServer};
use super::oauth::{self, Tokens};
use crate::app::{self, Request, TodoApp};
use crate::client::{self, Client};

/// A refused token straight after a refresh means signing in again won't
/// help: the server doesn't trust the authority's tokens.
const REFRESH_GRACE: Duration = Duration::from_secs(30);

pub struct Shell {
    ctx: egui::Context,
    config: Config,
    stage: Stage,
    /// The server URL typed in the chooser.
    url: String,
    /// Why the chooser is showing again.
    message: Option<String>,
}

enum Stage {
    Choose,
    Waiting {
        what: &'static str,
        done: Receiver<Result<Opened, String>>,
    },
    Running {
        app: Box<TodoApp>,
        session: Session,
    },
}

/// A board ready to show.
enum Opened {
    Local(LocalServer),
    Remote { url: String, tokens: Tokens },
}

enum Session {
    /// Stops the local server when dropped.
    Local { _server: LocalServer },
    Remote {
        url: String,
        tokens: Tokens,
        refreshing: Option<Receiver<Result<Tokens, String>>>,
        refreshed: Option<Instant>,
    },
}

impl Shell {
    /// Opens the board chosen last time, or asks where it is.
    pub fn new(cc: &eframe::CreationContext) -> Self {
        app::setup(&cc.egui_ctx);
        let config = Config::load();
        let mut shell = Shell {
            ctx: cc.egui_ctx.clone(),
            config: config.clone(),
            stage: Stage::Choose,
            url: String::new(),
            message: None,
        };
        match config.source {
            None => {}
            Some(Source::Local) => shell.open_local(),
            Some(Source::Server { url, refresh_token }) => {
                shell.url = url.clone();
                match (refresh_token, normalize(&url)) {
                    // Saved before plain http:// was refused: not sent there.
                    (_, Err(e)) => shell.message = Some(e),
                    (Some(token), Ok(_)) => shell.wait("Signing in…", move || {
                        let tokens = oauth::refresh(&url, &token)
                            .map_err(|e| format!("Sign in again ({e})."))?;
                        Ok(Opened::Remote { url, tokens })
                    }),
                    (None, Ok(_)) => shell.message = Some("Sign in again.".into()),
                }
            }
        }
        shell
    }

    /// Does `job` on another thread, showing `what` until it's done.
    fn wait(
        &mut self,
        what: &'static str,
        job: impl FnOnce() -> Result<Opened, String> + Send + 'static,
    ) {
        let (tx, done) = mpsc::channel();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
        self.message = None;
        self.stage = Stage::Waiting { what, done };
    }

    fn open_local(&mut self) {
        self.wait("Opening the local board…", || {
            local::start().map(Opened::Local)
        });
    }

    fn sign_in(&mut self) {
        let url = match normalize(&self.url) {
            Ok(url) => url,
            Err(e) => {
                self.message = Some(e);
                return;
            }
        };
        self.url = url.clone();
        self.wait("Sign in in your browser…", move || {
            let tokens = oauth::sign_in(&url)?;
            Ok(Opened::Remote { url, tokens })
        });
    }

    /// Shows the board just opened, and remembers where it is.
    fn opened(&mut self, opened: Opened) {
        let ctx = self.ctx.clone();
        let wake = move || ctx.request_repaint();
        let (client, session, source) = match opened {
            Opened::Local(server) => (
                Client::new(
                    &server.url,
                    Some(server.secret.clone()),
                    client::TIMEOUT,
                    wake,
                ),
                Session::Local { _server: server },
                Source::Local,
            ),
            Opened::Remote { url, tokens } => (
                Client::new(&url, Some(tokens.access.clone()), client::TIMEOUT, wake),
                Session::Remote {
                    url: url.clone(),
                    refreshing: None,
                    refreshed: Some(Instant::now()),
                    tokens: tokens.clone(),
                },
                Source::Server {
                    url,
                    refresh_token: tokens.refresh,
                },
            ),
        };
        self.remember(Some(source));
        self.stage = Stage::Running {
            app: Box::new(TodoApp::with_client(client, true)),
            session,
        };
    }

    fn remember(&mut self, source: Option<Source>) {
        if self.config.source != source {
            self.config.source = source;
            self.config.save();
        }
    }

    /// Back to the chooser, forgetting the board, with `message` if any.
    fn choose(&mut self, message: Option<String>) {
        if let Stage::Running { app, .. } = &mut self.stage {
            app.client_mut().flush(Duration::from_secs(2));
        }
        let source = match &self.config.source {
            // Keep the server's address, without a way in, to sign in again.
            Some(Source::Server { url, .. }) => Some(Source::Server {
                url: url.clone(),
                refresh_token: None,
            }),
            _ => None,
        };
        self.remember(source);
        self.stage = Stage::Choose;
        self.message = message;
    }

    /// Keeps a remote session's token fresh: before it expires, and when
    /// the server stops taking it.
    fn keep_signed_in(&mut self) {
        let Stage::Running {
            app,
            session:
                Session::Remote {
                    url,
                    tokens,
                    refreshing,
                    refreshed,
                },
        } = &mut self.stage
        else {
            return;
        };
        if let Some(rx) = refreshing {
            match rx.try_recv() {
                Ok(Ok(new)) => {
                    *tokens = new;
                    *refreshing = None;
                    *refreshed = Some(Instant::now());
                    let client = app.client_mut();
                    client.set_token(Some(tokens.access.clone()));
                    client.fetch();
                    let source = Some(Source::Server {
                        url: url.clone(),
                        refresh_token: tokens.refresh.clone(),
                    });
                    self.remember(source);
                }
                Ok(Err(e)) => self.choose(Some(format!("Signed out: {e}"))),
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.choose(Some("Signed out.".into()));
                }
            }
            return;
        }
        let refused = app.client().unauthorized();
        if !refused && !tokens.expiring() {
            return;
        }
        let just_refreshed = refreshed.is_some_and(|at| at.elapsed() < REFRESH_GRACE);
        match (&tokens.refresh, refused && just_refreshed) {
            (Some(token), false) => {
                let (url, token) = (url.clone(), token.clone());
                let (tx, rx) = mpsc::channel();
                let ctx = self.ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(oauth::refresh(&url, &token));
                    ctx.request_repaint();
                });
                *refreshing = Some(rx);
            }
            (_, true) => self.choose(Some("The server doesn't accept this sign-in.".into())),
            (None, false) if refused => self.choose(Some("Signed out. Sign in again.".into())),
            // Expiring with no way to refresh: keep going until it's refused.
            (None, false) => {}
        }
    }

    fn chooser(&mut self, ui: &mut egui::Ui) {
        CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                let width = 320.0;
                ui.add_space((ui.available_height() / 2.0 - 160.0).max(16.0));
                ui.heading(RichText::new("Where's your board?").strong());
                ui.add_space(20.0);
                let local = ui
                    .add(
                        Button::new(RichText::new("Use a local board").strong())
                            .min_size(Vec2::new(width, 34.0)),
                    )
                    .on_hover_text("Kept in a file on this computer");
                if local.clicked() {
                    self.open_local();
                }
                ui.add_space(6.0);
                ui.weak("Kept on this computer only.");
                ui.add_space(36.0);
                ui.label(RichText::new("Or connect to a server").strong());
                ui.add_space(8.0);
                let field = ui.add(
                    TextEdit::singleline(&mut self.url)
                        .hint_text("https://its4tasks.example.com")
                        .desired_width(width),
                );
                let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.add_space(8.0);
                let ready = !self.url.trim().is_empty();
                let sign_in = ui.add_enabled(
                    ready,
                    Button::new("Sign in").min_size(Vec2::new(width, 30.0)),
                );
                if ready && (sign_in.clicked() || entered) {
                    self.sign_in();
                }
                ui.add_space(6.0);
                ui.weak("Signing in opens your browser.");
                if let Some(message) = &self.message {
                    ui.add_space(16.0);
                    ui.colored_label(egui::Color32::from_rgb(200, 55, 45), message);
                }
            });
        });
    }

    fn waiting(ui: &mut egui::Ui, what: &str) -> bool {
        let mut cancel = false;
        CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space((ui.available_height() / 2.0 - 40.0).max(16.0));
                ui.spinner();
                ui.add_space(8.0);
                ui.label(what);
                ui.add_space(12.0);
                cancel = ui.button("Cancel").clicked();
            });
        });
        cancel
    }
}

/// `example.com/` → `https://example.com`. Plain `http://` only reaches
/// this computer: elsewhere, anyone on the way could read the tokens sent.
fn normalize(url: &str) -> Result<String, String> {
    let url = url.trim().trim_end_matches('/');
    let url = if url.contains("://") {
        url.to_owned()
    } else {
        format!("https://{url}")
    };
    if let Some(rest) = url.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or("");
        let host = match authority.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or(""),
            None => authority.split(':').next().unwrap_or(""),
        };
        let loopback = host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        if !loopback {
            return Err("Use https:// for a server on another computer.".into());
        }
    } else if !url.starts_with("https://") {
        return Err("The server's address starts with https://.".into());
    }
    Ok(url)
}

impl App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        match &mut self.stage {
            Stage::Choose => self.chooser(ui),
            Stage::Waiting { what, done } => {
                let what = *what;
                match done.try_recv() {
                    Ok(Ok(opened)) => {
                        self.opened(opened);
                        ui.ctx().request_repaint();
                    }
                    Ok(Err(e)) => self.choose(Some(e)),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.choose(Some("That didn't work.".into()));
                    }
                    Err(mpsc::TryRecvError::Empty) => {
                        if Self::waiting(ui, what) {
                            // The work goes on, unheard: the sign-in
                            // listener gives up on its own.
                            self.choose(None);
                        }
                    }
                }
            }
            Stage::Running { app, .. } => {
                app.ui(ui, frame);
                if app.take_request() == Some(Request::SwitchBoard) {
                    self.choose(None);
                } else {
                    self.keep_signed_in();
                }
            }
        }
    }

    fn on_exit(&mut self) {
        if let Stage::Running { app, .. } = &mut self.stage {
            app.on_exit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_are_tidied() {
        assert_eq!(normalize(" example.com/ ").unwrap(), "https://example.com");
        for local in [
            "http://127.0.0.1:7979",
            "http://localhost:7979",
            "http://[::1]:7979",
        ] {
            assert_eq!(normalize(local).unwrap(), local);
        }
        for refused in [
            "http://example.com",
            "http://127.0.0.1.example.com",
            "http://192.168.1.2:7979",
            "ftp://example.com",
        ] {
            assert!(normalize(refused).is_err(), "{refused}");
        }
    }
}
