//! `galley.toml`. The defaults follow SPEC.md §9.3. Galley parses the sections that later
//! milestones use, so a config written today keeps working.

use std::path::{Path, PathBuf};
use std::time::Duration;

use galley_build::EngineKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub build: BuildConfig,
    pub sync: SyncSection,
    pub grammar: GrammarConfig,
    pub backup: BackupConfig,
}

/// Continuous backup to an S3-compatible bucket. Off until a bucket is set, here or through the
/// environment. The keys are best kept out of this file: `GALLEY_BACKUP_ACCESS_KEY_ID` and
/// `GALLEY_BACKUP_SECRET_ACCESS_KEY`, or the standard `AWS_*` variables.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupConfig {
    /// For example `https://<account>.r2.cloudflarestorage.com`
    /// or `https://s3.eu-central-1.amazonaws.com`.
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    /// Key prefix inside the bucket, so one bucket can hold several servers.
    pub prefix: String,
    /// Path-style URLs, which MinIO and some self-hosted stores need.
    pub path_style: bool,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// How often a pass looks for changed projects. Work since the last pass is what a lost
    /// volume can cost.
    pub interval_s: u64,
}

impl Default for BackupConfig {
    fn default() -> Self {
        BackupConfig {
            endpoint: String::new(),
            bucket: String::new(),
            region: "auto".into(),
            prefix: "galley".into(),
            path_style: false,
            access_key_id: String::new(),
            secret_access_key: String::new(),
            interval_s: 120,
        }
    }
}

impl std::fmt::Debug for BackupConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupConfig")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("region", &self.region)
            .field("prefix", &self.prefix)
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field("interval_s", &self.interval_s)
            .finish()
    }
}

/// Backup settings after the environment fills the gaps the file left.
pub struct ResolvedBackup {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl BackupConfig {
    /// `None` when backup is off. The file wins over the environment, `GALLEY_BACKUP_*` wins over
    /// the standard `AWS_*` names and `BUCKET_NAME`, which some hosts set when they attach a bucket.
    pub fn resolve(&self) -> Option<ResolvedBackup> {
        self.resolve_with(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    fn resolve_with(&self, env: impl Fn(&str) -> Option<String>) -> Option<ResolvedBackup> {
        let pick = |own: &str, names: &[&str]| -> Option<String> {
            if !own.is_empty() {
                return Some(own.to_string());
            }
            names.iter().find_map(|n| env(n))
        };
        let bucket = pick(&self.bucket, &["GALLEY_BACKUP_BUCKET", "BUCKET_NAME"])?;
        Some(ResolvedBackup {
            endpoint: pick(&self.endpoint, &["GALLEY_BACKUP_ENDPOINT", "AWS_ENDPOINT_URL_S3"])
                .unwrap_or_else(|| "https://s3.amazonaws.com".into()),
            region: if self.region.is_empty() || self.region == "auto" {
                pick("", &["GALLEY_BACKUP_REGION", "AWS_REGION"]).unwrap_or_else(|| "auto".into())
            } else {
                self.region.clone()
            },
            access_key_id: pick(&self.access_key_id, &["GALLEY_BACKUP_ACCESS_KEY_ID", "AWS_ACCESS_KEY_ID"])?,
            secret_access_key: pick(&self.secret_access_key, &["GALLEY_BACKUP_SECRET_ACCESS_KEY", "AWS_SECRET_ACCESS_KEY"])?,
            bucket,
        })
    }
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
    #[serde(deserialize_with = "deserialize_engine")]
    pub engine: EngineKind,
    /// Empty = downloaded into <data_dir>/tectonic on first use, or `tectonic` on $PATH.
    pub tectonic_path: String,
    pub texlive_path: String,
    pub timeout_s: u64,
    pub memory_mb: u64,
    pub cpus: f32,
    /// Whether a new browser builds on its own after an edit, until its user picks a mode. It is
    /// off by default: on a shared server, a build on every pause is most of the compile load.
    pub auto_build: bool,
    /// How long typing must pause before an auto build starts. Shorter feels livelier and costs
    /// a compile for nearly every sentence.
    pub auto_build_delay_s: u64,
    /// auto | bwrap | docker | none
    pub sandbox: String,
    pub docker_image: String,
    /// Allow public mode (a set `server.domain`) even when no compile sandbox is available.
    /// It is off by default, because an unsandboxed compiler must not face the internet. Set it
    /// only for a deployment whose users you trust, such as a private test group. Understand the
    /// risk before you set it.
    pub allow_unsandboxed: bool,
    /// Compiles that may run at once across the whole server. Set it so that this many times
    /// `memory_mb` fits in the machine's memory with room for the server itself.
    pub max_concurrent: usize,
    /// Send compiles to build workers (`galley worker`) at this address, for example
    /// `http://galley-compile.flycast`. Empty compiles on this machine.
    pub worker_url: String,
    /// The shared secret between the server and its workers. Prefer `GALLEY_WORKER_TOKEN`.
    pub worker_token: Secret,
}

/// A secret from `galley.toml`. It never appears in `Debug` output or logs.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() { "<unset>" } else { "<redacted>" })
    }
}

impl Secret {
    /// The value from the environment variable `var`, else from the file, else none.
    pub fn resolve(&self, var: &str) -> Option<String> {
        std::env::var(var)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| Some(self.0.clone()).filter(|v| !v.trim().is_empty()))
    }
}

impl Default for BuildConfig {
    fn default() -> Self {
        BuildConfig {
            engine: EngineKind::Tectonic,
            tectonic_path: String::new(),
            texlive_path: String::new(),
            timeout_s: 120,
            memory_mb: 2048,
            cpus: 2.0,
            auto_build: false,
            auto_build_delay_s: 10,
            sandbox: "auto".into(),
            docker_image: "debian:bookworm-slim".into(),
            worker_url: String::new(),
            worker_token: Secret::default(),
            allow_unsandboxed: false,
            max_concurrent: 2,
        }
    }
}

fn deserialize_engine<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<EngineKind, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value == "texlive" {
        return Ok(EngineKind::PdfLatex);
    }
    serde_json::from_value(serde_json::Value::String(value)).map_err(serde::de::Error::custom)
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
        assert_eq!(parsed.build.engine, EngineKind::Tectonic);
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let parsed: Config = toml::from_str("[server]\nbind = \"0.0.0.0:8080\"\n").unwrap();
        assert_eq!(parsed.server.bind, "0.0.0.0:8080");
        assert_eq!(parsed.sync.flush_max_ms, 60_000);
    }

    #[test]
    fn backup_is_off_until_a_bucket_and_keys_exist() {
        let none = |_: &str| None;
        assert!(BackupConfig::default().resolve_with(none).is_none());
        let only_bucket = BackupConfig { bucket: "b".into(), ..BackupConfig::default() };
        assert!(only_bucket.resolve_with(none).is_none(), "no keys, no backup");
    }

    #[test]
    fn backup_reads_the_standard_variables() {
        let env = |k: &str| {
            match k {
                "BUCKET_NAME" => Some("galley-backup"),
                "AWS_ENDPOINT_URL_S3" => Some("https://storage.example.com"),
                "AWS_ACCESS_KEY_ID" => Some("tid_x"),
                "AWS_SECRET_ACCESS_KEY" => Some("tsec_y"),
                "GALLEY_BACKUP_SECRET_ACCESS_KEY" => Some("own"),
                _ => None,
            }
            .map(str::to_string)
        };
        let r = BackupConfig::default().resolve_with(env).unwrap();
        assert_eq!(r.bucket, "galley-backup");
        assert_eq!(r.endpoint, "https://storage.example.com");
        assert_eq!(r.access_key_id, "tid_x");
        assert_eq!(r.secret_access_key, "own", "the Galley name wins over the generic one");
        assert_eq!(r.region, "auto");
        let shown = format!("{:?}", BackupConfig { secret_access_key: "s3cr3t".into(), ..BackupConfig::default() });
        assert!(!shown.contains("s3cr3t"));
    }

    #[test]
    fn auto_build_starts_off_with_a_long_pause() {
        let parsed: Config = toml::from_str("").unwrap();
        assert!(!parsed.build.auto_build);
        assert_eq!(parsed.build.auto_build_delay_s, 10);
        let parsed: Config = toml::from_str("[build]\nauto_build = true\nauto_build_delay_s = 20\n").unwrap();
        assert!(parsed.build.auto_build);
        assert_eq!(parsed.build.auto_build_delay_s, 20);
    }

    #[test]
    fn legacy_texlive_config_selects_pdflatex() {
        let parsed: Config = toml::from_str("[build]\nengine = \"texlive\"\n").unwrap();
        assert_eq!(parsed.build.engine, EngineKind::PdfLatex);
        let parsed: Config = toml::from_str("[build]\nengine = \"xelatex\"\n").unwrap();
        assert_eq!(parsed.build.engine, EngineKind::XeLatex);
    }
}
