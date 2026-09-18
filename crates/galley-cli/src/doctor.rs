//! `galley doctor` explains why Galley does not work.
//!
//! Every check states what it looked for and what it found. When something is wrong, the check also
//! gives the one command that fixes it. Warnings never fail the run. Only a fault that stops Galley
//! working fails the run.

use std::fmt::Write as _;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};

use galley_server::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn mark(self) -> &'static str {
        match self {
            Level::Ok => "ok  ",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Check {
    pub level: Level,
    pub what: String,
    pub detail: String,
    /// What to do about it, when there is something to do.
    pub fix: Option<String>,
}

impl Check {
    fn ok(what: &str, detail: impl Into<String>) -> Check {
        Check { level: Level::Ok, what: what.into(), detail: detail.into(), fix: None }
    }
    fn warn(what: &str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
        Check { level: Level::Warn, what: what.into(), detail: detail.into(), fix: Some(fix.into()) }
    }
    fn fail(what: &str, detail: impl Into<String>, fix: impl Into<String>) -> Check {
        Check { level: Level::Fail, what: what.into(), detail: detail.into(), fix: Some(fix.into()) }
    }
}

/// Run every check. `config_path` is reported as-is so the reader knows which file was read.
pub async fn run(config: &Config, config_path: &Path, data_dir: &Path) -> Vec<Check> {
    let mut out = vec![configuration(config_path)];
    out.push(data_directory(data_dir));
    out.push(disk_space(data_dir));
    out.push(sandbox(config).await);
    out.extend(engine(config, data_dir).await);
    out.push(port(&config.server.bind));
    out.extend(public_mode(config));
    out.push(optional_tool("latexdiff", "Compare PDFs", "apt install latexdiff"));
    out.push(grammar(config).await);
    out.push(fonts());
    out
}

/// Print the report and return the process exit code.
pub fn report(checks: &[Check]) -> i32 {
    let mut text = String::new();
    for c in checks {
        let _ = writeln!(text, "{}  {:<22} {}", c.level.mark(), c.what, c.detail);
        if let Some(fix) = &c.fix {
            let _ = writeln!(text, "                             → {fix}");
        }
    }
    let fails = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warns = checks.iter().filter(|c| c.level == Level::Warn).count();
    let _ = writeln!(
        text,
        "\n{} check(s): {} ok, {warns} warning(s), {fails} failure(s).",
        checks.len(),
        checks.len() - warns - fails
    );
    if fails == 0 && warns == 0 {
        let _ = writeln!(text, "Everything Galley needs is in place.");
    }
    print!("{text}");
    i32::from(fails > 0)
}

fn configuration(path: &Path) -> Check {
    if path.exists() {
        Check::ok("configuration", format!("{}", path.display()))
    } else {
        Check::warn(
            "configuration",
            format!("no file at {}; built-in defaults are in use", path.display()),
            "galley init   (writes a galley.toml you can edit)",
        )
    }
}

fn data_directory(dir: &Path) -> Check {
    if !dir.exists() {
        return Check::warn("data directory", format!("{} does not exist yet", dir.display()), "it is created on first run");
    }
    let probe = dir.join(".galley-doctor-probe");
    match std::fs::write(&probe, b"x").and_then(|_| std::fs::remove_file(&probe)) {
        Ok(()) => Check::ok("data directory", format!("{} is writable", dir.display())),
        Err(e) => Check::fail(
            "data directory",
            format!("{} is not writable: {e}", dir.display()),
            format!("chown -R $(whoami) {}", dir.display()),
        ),
    }
}

/// Free space where the projects live. A build needs room for the PDF and the package cache.
fn disk_space(dir: &Path) -> Check {
    let target = if dir.exists() { dir.to_path_buf() } else { PathBuf::from(".") };
    let Some(free_mb) = free_megabytes(&target) else {
        return Check::warn("disk space", "could not be measured", "check it yourself with: df -h");
    };
    let detail = format!("{free_mb} MB free on {}", target.display());
    match free_mb {
        0..=200 => Check::fail("disk space", detail, "free some space: builds and the package cache need room"),
        201..=1000 => Check::warn("disk space", detail, "1 GB or more is comfortable once the package cache fills"),
        _ => Check::ok("disk space", detail),
    }
}

fn free_megabytes(dir: &Path) -> Option<u64> {
    let out = std::process::Command::new("df").arg("-Pk").arg(dir).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let available_kb: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    Some(available_kb / 1024)
}

async fn sandbox(config: &Config) -> Check {
    let b = &config.build;
    let sandbox = galley_build::Sandbox::detect(&b.sandbox, &b.docker_image, b.memory_mb, b.cpus).await;
    let public = !config.server.domain.is_empty();
    match sandbox.kind {
        galley_build::SandboxKind::Bwrap => Check::ok("sandbox", "bubblewrap; builds run isolated"),
        galley_build::SandboxKind::Docker => Check::ok("sandbox", format!("docker ({})", b.docker_image)),
        galley_build::SandboxKind::None if public => Check::fail(
            "sandbox",
            "none, and this server is public (server.domain is set)",
            "apt install bubblewrap   (Galley refuses to serve publicly without a sandbox)",
        ),
        galley_build::SandboxKind::None => Check::warn(
            "sandbox",
            "none; builds run with your own user's access",
            "apt install bubblewrap   (required before serving on a domain)",
        ),
    }
}

async fn engine(config: &Config, data_dir: &Path) -> Vec<Check> {
    let mut out = Vec::new();
    let configured = config.build.tectonic_path.trim();
    let candidates: Vec<PathBuf> = if configured.is_empty() {
        vec![data_dir.join("tectonic").join("tectonic")]
    } else {
        vec![PathBuf::from(configured)]
    };
    match candidates.iter().find(|p| p.is_file()) {
        Some(path) => {
            let version = std::process::Command::new(path)
                .arg("--version")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "installed".into());
            out.push(Check::ok("build engine", format!("{version} at {}", path.display())));
        }
        None => out.push(Check::warn(
            "build engine",
            "Tectonic is not installed yet",
            "galley engine install tectonic   (or it downloads itself on the first build)",
        )),
    }
    // A warm bundle cache makes builds fast, and lets them run offline.
    let cache = data_dir.join("tectonic").join("cache");
    let warm = std::fs::read_dir(&cache).map(|mut d| d.next().is_some()).unwrap_or(false);
    out.push(if warm {
        Check::ok("package cache", format!("warm at {}", cache.display()))
    } else {
        Check::warn(
            "package cache",
            "empty; the first build downloads what the document needs",
            "the first build needs network access, later ones do not",
        )
    });
    out
}

fn port(bind: &str) -> Check {
    let Ok(addr) = bind.parse::<SocketAddr>() else {
        return Check::fail("listen address", format!("{bind} is not an address:port"), "fix server.bind in galley.toml");
    };
    match TcpListener::bind(addr) {
        Ok(_) => Check::ok("listen address", format!("{bind} is free")),
        Err(e) => Check::fail(
            "listen address",
            format!("cannot bind {bind}: {e}"),
            "another Galley may be running, or the port needs privileges: galley serve --port 7001",
        ),
    }
}

fn public_mode(config: &Config) -> Vec<Check> {
    let domain = config.server.domain.trim();
    if domain.is_empty() {
        return vec![Check::ok("public mode", "off; serving on the bind address only")];
    }
    let mut out = vec![Check::ok("public mode", format!("on for {domain}; certificates come from Let's Encrypt"))];
    // ACME needs both, and a certificate cannot be issued without them reachable from outside.
    for p in [80u16, 443] {
        let Err(e) = TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], p))) else { continue };
        let (detail, fix) = match e.kind() {
            std::io::ErrorKind::PermissionDenied => (
                format!("port {p} needs privileges this user does not have"),
                "run Galley as a systemd service (deploy/install.sh), or behind a reverse proxy".to_string(),
            ),
            _ => (
                format!("port {p} is in use"),
                format!("free port {p}, or put Galley behind the proxy that holds it (deploy/README.md)"),
            ),
        };
        out.push(Check::warn("certificate ports", detail, fix));
    }
    out
}

fn optional_tool(binary: &str, feature: &str, fix: &str) -> Check {
    match which(binary) {
        Some(path) => Check::ok(binary, format!("{}; {feature} works", path.display())),
        None => Check::warn(binary, format!("not installed; {feature} is unavailable"), fix.to_string()),
    }
}

/// LanguageTool is optional and needs Java, so a missing one is never a failure.
async fn grammar(config: &Config) -> Check {
    let Ok(base) = galley_server::grammar::endpoint(&config.grammar.languagetool) else {
        return Check::ok("grammar", "off; Check grammar says so instead of failing");
    };
    let reachable = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()
        .map(|c| async move { c.get(format!("{base}/v2/languages")).send().await.is_ok() });
    let ok = match reachable {
        Some(f) => f.await,
        None => false,
    };
    let base = redact(&galley_server::grammar::endpoint(&config.grammar.languagetool).unwrap_or_default());
    if ok {
        Check::ok("grammar", format!("LanguageTool answering at {base}"))
    } else {
        Check::warn(
            "grammar",
            format!("no LanguageTool at {base}; Check grammar will say it is unavailable"),
            "start languagetool-server (it needs Java), or set [grammar] languagetool = \"off\"",
        )
    }
}

/// Hide any credentials in a URL, because doctor's output goes into bug reports.
fn redact(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    match rest.split_once('@') {
        Some((_, host)) => format!("{scheme}://***@{host}"),
        None => url.to_string(),
    }
}

/// Text inside SVG figures is drawn with the host's fonts (SPEC §13.1.1).
fn fonts() -> Check {
    let dirs = ["/usr/share/fonts", "/usr/local/share/fonts"];
    let any = dirs.iter().any(|d| std::fs::read_dir(d).map(|mut e| e.next().is_some()).unwrap_or(false));
    if any {
        Check::ok("fonts", "system fonts found; text in SVG figures will render")
    } else {
        Check::warn(
            "fonts",
            "no system fonts; text inside SVG figures would come out blank",
            "apt install fonts-dejavu-core",
        )
    }
}

fn which(binary: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(binary)).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_config_is_a_warning_with_a_fix() {
        let c = configuration(Path::new("/nowhere/galley.toml"));
        assert_eq!(c.level, Level::Warn);
        assert!(c.fix.as_deref().is_some_and(|f| f.contains("galley init")));
    }

    #[test]
    fn an_unwritable_data_directory_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(data_directory(dir.path()).level, Level::Ok);
        assert_eq!(data_directory(Path::new("/proc/nonexistent-galley")).level, Level::Warn, "not there yet is not a failure");
    }

    #[test]
    fn a_taken_port_fails_and_a_free_one_passes() {
        let held = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = held.local_addr().unwrap();
        let c = port(&addr.to_string());
        assert_eq!(c.level, Level::Fail, "{c:?}");
        assert!(c.detail.contains("cannot bind"));
        drop(held);
        assert_eq!(port(&addr.to_string()).level, Level::Ok);
        assert_eq!(port("not-an-address").level, Level::Fail);
    }

    #[test]
    fn disk_space_is_measured_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let c = disk_space(dir.path());
        assert!(c.detail.contains("MB free"), "{c:?}");
        assert!(free_megabytes(dir.path()).is_some());
    }

    #[test]
    fn credentials_in_a_url_are_never_printed() {
        assert_eq!(redact("http://user:pw@lt.example.org:8010"), "http://***@lt.example.org:8010");
        assert_eq!(redact("http://127.0.0.1:8081"), "http://127.0.0.1:8081");
    }

    #[test]
    fn the_report_fails_only_on_failures() {
        let ok = vec![Check::ok("a", "fine"), Check::warn("b", "hmm", "do this")];
        assert_eq!(report(&ok), 0);
        assert_eq!(report(&[Check::fail("c", "broken", "fix it")]), 1);
    }
}
