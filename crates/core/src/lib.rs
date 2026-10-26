//! Client library shared by the CLI and the desktop app: connection (SSH tunnel), the agent's API, projects,
//! delta sync, resumable downloads and the one-time VPS setup.

pub mod client;
pub mod config;
pub mod download;
pub mod registry;
pub mod render;
pub mod setup;
pub mod sync;
pub mod transfer;

pub use client::{Client, Tunnel};
pub use config::{config_path, data_dir, Config};
pub use transfer::{Cancelled, Phase, Snapshot, Transfer};

/// A child-process command that never flashes a console window when launched from a GUI on Windows.
pub fn command(program: &str) -> tokio::process::Command {
    #[allow(unused_mut)]
    let mut c = tokio::process::Command::new(program);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}
