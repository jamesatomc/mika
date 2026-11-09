//! Mika common helpers — config loading (kanari-common style).
//!
//! All user-facing tools read configuration from **MIKA_HOME** (env override)
//! or `~/.mika` by default. The only config file is `<MIKA_HOME>/mika.yaml`
//! with `key: value` lines (serde_yaml):
//!
//! ```yaml
//! # <MIKA_HOME>/mika.yaml
//! rpc: http://192.168.1.100:19001
//! ```
//!
//! The mesh map (multiaddr registry) lives at `<MIKA_HOME>/mesh` —
//! `resolve_multiaddr` in `mika_server::p2p` reads the same path, so a
//! single location covers all binaries.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// Mika user config: resolved paths only (everything else is derived).
#[derive(Debug, Clone)]
pub struct MikaConfig {
    /// User home dir (`MIKA_HOME` or `~/.mika`).
    pub home: PathBuf,
    /// Mesh map file (`<home>/mesh`) — `host:port -> multiaddr` registry.
    pub mesh_file: PathBuf,
}

impl MikaConfig {
    /// Load the config for this user (defaults when `mika.yaml` is absent).
    pub fn load() -> Result<Self> {
        let home = home_dir()?;
        let cfg_path = home.join("mika.yaml");
        if cfg_path.exists() {
            let _raw = std::fs::read_to_string(&cfg_path)
                .with_context(|| format!("reading {}", cfg_path.display()))?;
        }
        Ok(Self {
            mesh_file: home.join("mesh"),
            home,
        })
    }
}

/// User home dir: `MIKA_HOME` env var wins; fall back to `~/.mika`.
pub fn home_dir() -> Result<PathBuf> {
    if let Ok(h) = std::env::var("MIKA_HOME") {
        return Ok(PathBuf::from(h));
    }
    if let Ok(h) = std::env::var("HOME") {
        return Ok(PathBuf::from(h).join(".mika"));
    }
    if let Ok(h) = std::env::var("USERPROFILE") {
        return Ok(PathBuf::from(h).join(".mika"));
    }
    Err(anyhow::anyhow!("no MIKA_HOME and no HOME/USERPROFILE set"))
}

/// Mesh map file path (`<MIKA_HOME>/mesh`).
pub fn mesh_map_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("mesh"))
}

#[cfg(test)]
mod tests;
