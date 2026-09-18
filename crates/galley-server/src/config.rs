//! `galley.toml`. The defaults follow SPEC.md §9.3. Galley parses the sections that later
//! milestones use, so a config written today keeps working.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub build: BuildConfig,
    pub sync: SyncSection,
    pub grammar: GrammarConfig,
}

/// LanguageTool needs Java and a running server, so Galley never requires it. When it is off, the
/// Proofread button says so instead of failing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GrammarConfig {
    /// `off`, `auto` (a LanguageTool on this machine's default port), or a server URL.
    pub languagetool: String,
    /// A LanguageTool language code, or `auto` to let it detect one.
    pub language: String,
}

impl Default for GrammarConfig {
    fn default() -> Self {
        GrammarConfig {
            languagetool: "off".into(),
            language: "auto".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Galley has no login until accounts arrive in M3, so it listens on loopback by default.
    pub bind: String,
    pub domain: String,
    pub data_dir: String,
    pub public_signup: bool,
    /// An address for the polite pools of Crossref and OpenAlex. An empty value means anonymous.
    #[serde(default)]
    pub contact_email: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            bind: "127.0.0.1:7000".into(),
            domain: String::new(),
            data_dir: "~/.galley".into(),
            public_signup: false,
            contact_email: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildConfig {
    pub engine: String,
    /// Empty = downloaded into <data_dir>/tectonic on first use, or `tectonic` on $PATH.
    pub tectonic_path: String,
    pub texlive_path: String,
    pub timeout_s: u64,
    pub memory_mb: u64,
    pub cpus: f32,
    pub auto_build: bool,
    /// auto | bwrap | docker | none
    pub sandbox: String,
    pub docker_image: String,
    /// Allow public mode (a set `server.domain`) even when no compile sandbox is available.
    /// It is off by default, because an unsandboxed compiler must not face the internet. Set it
    /// only for a deployment whose users you trust, such as a private test group. Understand the
    /// risk before you set it.
    pub allow_unsandboxed: bool,
}

impl Default for BuildConfig {
    fn default() -> Self {
        BuildConfig {
            engine: "tectonic".into(),
            tectonic_path: String::new(),
            texlive_path: String::new(),
            timeout_s: 120,
            memory_mb: 2048,
            cpus: 2.0,
            auto_build: true,
            sandbox: "auto".into(),
            docker_image: "debian:bookworm-slim".into(),
            allow_unsandboxed: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncSection {
    pub flush_quiet_ms: u64,
    pub flush_max_ms: u64,
}

impl Default for SyncSection {
    fn default() -> Self {
        SyncSection {
            flush_quiet_ms: 4000,
            flush_max_ms: 60_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl Config {
    /// Load from `path`, or defaults when the file does not exist.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    pub fn default_path() -> PathBuf {
        expand_home("~/.galley").join("galley.toml")
    }

    pub fn data_dir(&self) -> PathBuf {
        expand_home(&self.server.data_dir)
    }

    pub fn sync_config(&self) -> galley_sync::SyncConfig {
        galley_sync::SyncConfig {
            flush_quiet: Duration::from_millis(self.sync.flush_quiet_ms),
            flush_max: Duration::from_millis(self.sync.flush_max_ms),
            ..galley_sync::SyncConfig::default()
        }
    }

    /// The commented default file the installer writes.
    pub fn default_toml() -> String {
        let mut out = String::from("# Galley configuration. Every key is optional.\n\n");
        out.push_str(&toml::to_string_pretty(&Config::default()).expect("defaults serialise"));
        out
    }
}

pub fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip() {
        let text = Config::default_toml();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.server.bind, "127.0.0.1:7000");
        assert_eq!(parsed.sync.flush_quiet_ms, 4000);
        assert_eq!(parsed.build.engine, "tectonic");
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let parsed: Config = toml::from_str("[server]\nbind = \"0.0.0.0:8080\"\n").unwrap();
        assert_eq!(parsed.server.bind, "0.0.0.0:8080");
        assert_eq!(parsed.sync.flush_max_ms, 60_000);
    }
}
