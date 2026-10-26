use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub host: String,
    pub user: String,
    pub key: Option<String>,
    pub ssh_port: u16,
    #[serde(default)]
    pub token: String,
    /// Port the agent listens on, on the VPS (127.0.0.1).
    pub remote_port: u16,
    /// Local port the SSH tunnel binds.
    pub local_port: u16,
    /// Skip the SSH tunnel and talk to this URL directly (local testing).
    pub direct_url: Option<String>,
    /// Where downloaded renders go. Default: `Downloads/HFR`.
    #[serde(default)]
    pub download_dir: Option<String>,
    /// Delete a render from the VPS right after it was downloaded and verified.
    #[serde(default = "yes")]
    pub free_after_download: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            host: String::new(),
            user: "root".into(),
            key: None,
            ssh_port: 22,
            token: String::new(),
            remote_port: 7878,
            local_port: 17878,
            direct_url: None,
            download_dir: None,
            free_after_download: true,
        }
    }
}

/// `HFR_CONFIG` selects another config file (development and tests). A config with `direct_url` never touches
/// the OS credential store, so test setups cannot overwrite a real token.
pub fn config_path() -> PathBuf {
    if let Some(p) = std::env::var_os("HFR_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("hfr").join("config.toml")
}

/// Folder next to the config file for the app's own data (project registry, hash cache).
pub fn data_dir() -> PathBuf {
    config_path().parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
}

/// The token lives in the OS credential store when one is available (Windows Credential Manager, macOS Keychain),
/// and otherwise falls back to the config file, which is then restricted to the owner on Unix.
#[cfg(any(windows, target_os = "macos"))]
mod secret {
    fn entry() -> Option<keyring::Entry> {
        keyring::Entry::new("hfr", "agent-token").ok()
    }
    pub fn store(token: &str) -> bool {
        entry().map(|e| e.set_password(token).is_ok()).unwrap_or(false)
    }
    pub fn fetch() -> Option<String> {
        entry()?.get_password().ok()
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod secret {
    pub fn store(_: &str) -> bool {
        false
    }
    pub fn fetch() -> Option<String> {
        None
    }
}

impl Config {
    pub fn load() -> Result<Config> {
        let p = config_path();
        let s = std::fs::read_to_string(&p).with_context(|| format!("no config at {} - run `hfr init` first", p.display()))?;
        let mut cfg: Config = toml::from_str(&s)?;
        if cfg.direct_url.is_some() {
            return Ok(cfg);
        }
        if cfg.token.is_empty() {
            cfg.token = secret::fetch().context("token not found in the credential store - run `hfr setup` or `hfr init` again")?;
        } else if secret::store(&cfg.token) {
            // Migrate a plaintext token from an older config into the credential store.
            cfg.save()?;
        }
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let p = config_path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        let mut on_disk = self.clone();
        if self.direct_url.is_none() && !self.token.is_empty() && secret::store(&self.token) {
            on_disk.token = String::new();
        }
        std::fs::write(&p, toml::to_string_pretty(&on_disk)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    /// Folder downloads are saved into.
    pub fn download_folder(&self) -> PathBuf {
        match &self.download_dir {
            Some(d) if !d.trim().is_empty() => PathBuf::from(d),
            _ => dirs::download_dir().unwrap_or_else(|| PathBuf::from(".")).join("HFR"),
        }
    }
}
