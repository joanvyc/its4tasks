//! Where the native app's board is, remembered between runs in
//! `config.json` in the config directory (`~/.config/its4tasks` on Linux).
//! It holds a refresh token, so only its owner may read it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// `None` until chosen, and after switching.
    pub source: Option<Source>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The local board, in `board.sqlite` in the data directory.
    Local,
    /// A board on the server at `url`, signed in through the browser.
    Server {
        url: String,
        /// Signs in again without the browser.
        refresh_token: Option<String>,
    },
}

fn path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "its4tasks").map(|d| d.config_dir().join("config.json"))
}

impl Config {
    /// The saved configuration, or the default one if there's none yet.
    pub fn load() -> Config {
        let Some(path) = path() else {
            return Config::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("its4tasks: ignoring {}: {e}", path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = path() else {
            return;
        };
        if let Err(e) = write(
            &path,
            &serde_json::to_string_pretty(self).expect("serializes"),
        ) {
            eprintln!("its4tasks: failed to save {}: {e}", path.display());
        }
    }
}

/// Writes `text` to `path`, readable by its owner only.
fn write(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    std::io::Write::write_all(&mut options.open(path)?, text.as_bytes())
}
