//! One way to run a child process, with three backends. The project directory is mounted read-only
//! at `/work`. Only `.galley/build` is writable. The git internals and the Galley state are masked.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxKind {
    Bwrap,
    Docker,
    None,
}

impl std::fmt::Display for SandboxKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SandboxKind::Bwrap => "bwrap",
            SandboxKind::Docker => "docker",
            SandboxKind::None => "none",
        })
    }
}

#[derive(Debug, Clone)]
pub struct Sandbox {
    pub kind: SandboxKind,
    pub docker_image: String,
    pub memory_mb: u64,
    pub cpus: f32,
    /// `systemd-run --user --scope` is available for cgroup limits under bwrap.
    pub systemd_scope: bool,
}

/// A host directory or file made visible inside the sandbox.
#[derive(Debug, Clone)]
pub struct Mount {
    pub host: PathBuf,
    pub guest: PathBuf,
    pub writable: bool,
}

/// What to run. Paths in `args` must already be guest paths (see [`Mounts::guest`]).
#[derive(Debug, Clone)]
pub struct Spec {
    pub project_dir: PathBuf,
    pub out_dir: PathBuf,
    pub mounts: Vec<Mount>,
    /// Guest paths hidden behind an empty tmpfs even though they sit under a mounted directory.
    pub masked: Vec<PathBuf>,
    pub env: Vec<(String, String)>,
    pub network: bool,
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Container name so a timed-out docker run can be killed.
    pub name: String,
}

/// The map from host paths to guest paths for one spec.
pub struct Mounts {
    isolated: bool,
    pairs: Vec<(PathBuf, PathBuf)>,
}

impl Mounts {
    pub fn guest(&self, host: &Path) -> PathBuf {
        if !self.isolated {
            return host.to_path_buf();
        }
        let mut best: Option<(&PathBuf, &PathBuf)> = None;
        for (h, g) in &self.pairs {
            if host.starts_with(h) && best.is_none_or(|(bh, _)| h.components().count() > bh.components().count()) {
                best = Some((h, g));
            }
        }
        match best {
            Some((h, g)) => match host.strip_prefix(h) {
                Ok(rest) if rest.as_os_str().is_empty() => g.clone(),
                Ok(rest) => g.join(rest),
                Err(_) => host.to_path_buf(),
            },
            None => host.to_path_buf(),
        }
    }
}

pub const GUEST_WORK: &str = "/work";

impl Sandbox {
    /// Resolve `auto` with a probe, in the order of the spec: bwrap, then docker, then none.
    pub async fn detect(preference: &str, docker_image: &str, memory_mb: u64, cpus: f32) -> Sandbox {
        let systemd_scope = probe(
            "systemd-run",
            &["--user", "--scope", "-q", "-p", "MemoryMax=64M", "--", "/usr/bin/true"],
        )
        .await;
        let mk = |kind| Sandbox {
            kind,
            docker_image: docker_image.to_string(),
            memory_mb,
            cpus,
            systemd_scope,
        };
        match preference {
            "bwrap" => mk(SandboxKind::Bwrap),
            "docker" => mk(SandboxKind::Docker),
            "none" => mk(SandboxKind::None),
            _ => {
                if bwrap_works().await {
                    mk(SandboxKind::Bwrap)
                } else if probe("docker", &["info"]).await {
                    mk(SandboxKind::Docker)
                } else {
                    mk(SandboxKind::None)
                }
            }
        }
    }

    pub fn mounts(&self, project_dir: &Path, extra: &[Mount]) -> Mounts {
        let mut pairs = vec![(project_dir.to_path_buf(), PathBuf::from(GUEST_WORK))];
        pairs.extend(extra.iter().map(|m| (m.host.clone(), m.guest.clone())));
        Mounts {
            isolated: self.kind != SandboxKind::None,
            pairs,
        }
    }

    pub fn command(&self, spec: &Spec) -> Command {
        let mut cmd = match self.kind {
            SandboxKind::None => {
                let mut c = Command::new(&spec.program);
                c.args(&spec.args).current_dir(&spec.project_dir);
                for (k, v) in &spec.env {
                    c.env(k, v);
                }
                c
            }
            SandboxKind::Bwrap => self.bwrap(spec),
            SandboxKind::Docker => self.docker(spec),
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        cmd
    }

    fn bwrap(&self, spec: &Spec) -> Command {
        let mut argv: Vec<String> = Vec::new();
        if self.systemd_scope {
            argv.extend(
                [
                    "systemd-run",
                    "--user",
                    "--scope",
                    "-q",
                    "-p",
                    &format!("MemoryMax={}M", self.memory_mb),
                    "-p",
                    &format!("CPUQuota={}%", (self.cpus * 100.0).round() as u32),
                    "--",
                ]
                .map(String::from),
            );
        }
        argv.push("bwrap".into());
        for dir in ["/usr", "/lib", "/lib64", "/bin", "/sbin", "/etc/alternatives"] {
            if Path::new(dir).exists() {
                argv.extend(["--ro-bind", dir, dir].map(String::from));
            }
        }
        if spec.network {
            for f in ["/etc/resolv.conf", "/etc/ssl", "/etc/ca-certificates", "/etc/hosts"] {
                if Path::new(f).exists() {
                    argv.extend(["--ro-bind", f, f].map(String::from));
                }
            }
        }
        argv.extend(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"].map(String::from));
        argv.extend(["--ro-bind", &spec.project_dir.to_string_lossy(), GUEST_WORK].map(String::from));
        argv.extend(
            ["--bind", &spec.out_dir.to_string_lossy(), &format!("{GUEST_WORK}/.galley/build")].map(String::from),
        );
        for m in &spec.mounts {
            let flag = if m.writable { "--bind" } else { "--ro-bind" };
            argv.extend([flag, &m.host.to_string_lossy(), &m.guest.to_string_lossy()].map(String::from));
        }
        for p in &spec.masked {
            argv.extend(["--tmpfs", &p.to_string_lossy()].map(String::from));
        }
        argv.extend(["--unshare-user", "--unshare-pid", "--unshare-ipc", "--unshare-uts", "--unshare-cgroup"].map(String::from));
        if !spec.network {
            argv.push("--unshare-net".into());
        }
        argv.extend(["--die-with-parent", "--new-session", "--chdir", GUEST_WORK].map(String::from));
        for (k, v) in &spec.env {
            argv.extend(["--setenv", k, v].map(String::from));
        }
        argv.push("--".into());
        argv.push(spec.program.to_string_lossy().into_owned());
        argv.extend(spec.args.iter().cloned());
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]);
        c
    }

    fn docker(&self, spec: &Spec) -> Command {
        let mut c = Command::new("docker");
        c.args(["run", "--rm", "--init", "--name", &spec.name]);
        c.args(["--network", if spec.network { "bridge" } else { "none" }]);
        c.args(["--memory", &format!("{}m", self.memory_mb), "--cpus", &format!("{}", self.cpus)]);
        c.args(["--pids-limit", "512", "--security-opt", "no-new-privileges", "--cap-drop", "ALL"]);
        c.args(["--read-only", "--tmpfs", "/tmp:rw,exec,size=512m"]);
        c.args(["--user", &format!("{}:{}", uid(), gid())]);
        if spec.network {
            // A package fetch needs TLS. A slim image has no CA bundle, so mount the host bundle.
            for certs in ["/etc/ssl/certs", "/etc/ca-certificates"] {
                if Path::new(certs).is_dir() {
                    c.args(["-v", &format!("{certs}:{certs}:ro")]);
                }
            }
        }
        c.args(["-v", &format!("{}:{GUEST_WORK}:ro", spec.project_dir.display())]);
        c.args(["-v", &format!("{}:{GUEST_WORK}/.galley/build", spec.out_dir.display())]);
        for m in &spec.mounts {
            let ro = if m.writable { "" } else { ":ro" };
            c.args(["-v", &format!("{}:{}{ro}", m.host.display(), m.guest.display())]);
        }
        for p in &spec.masked {
            c.args(["--tmpfs", &p.to_string_lossy()]);
        }
        for (k, v) in &spec.env {
            c.args(["-e", &format!("{k}={v}")]);
        }
        c.args(["-w", GUEST_WORK, &self.docker_image]);
        c.arg(&spec.program);
        c.args(&spec.args);
        c
    }

    /// Clean up after a timeout if possible. A docker container keeps running after the client is
    /// killed.
    pub async fn kill(&self, spec: &Spec) {
        if self.kind == SandboxKind::Docker {
            let _ = Command::new("docker")
                .args(["kill", &spec.name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
}

async fn probe(program: &str, args: &[&str]) -> bool {
    let run = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(tokio::time::timeout(Duration::from_secs(10), run).await, Ok(Ok(s)) if s.success())
}

async fn bwrap_works() -> bool {
    let mut args = Vec::new();
    for dir in ["/usr", "/lib", "/lib64", "/bin"] {
        if Path::new(dir).exists() {
            args.extend(["--ro-bind", dir, dir]);
        }
    }
    args.extend(["--unshare-all", "--die-with-parent", "--", "/usr/bin/true"]);
    probe("bwrap", &args).await
}

fn uid() -> u32 {
    std::fs::metadata("/proc/self").map(|m| {
        use std::os::unix::fs::MetadataExt;
        m.uid()
    }).unwrap_or(1000)
}

fn gid() -> u32 {
    std::fs::metadata("/proc/self").map(|m| {
        use std::os::unix::fs::MetadataExt;
        m.gid()
    }).unwrap_or(1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(kind: SandboxKind) -> Sandbox {
        Sandbox {
            kind,
            docker_image: "debian:bookworm-slim".into(),
            memory_mb: 2048,
            cpus: 2.0,
            systemd_scope: false,
        }
    }

    fn spec(network: bool) -> Spec {
        Spec {
            project_dir: "/data/p1".into(),
            out_dir: "/data/p1/.galley/build".into(),
            mounts: vec![
                Mount { host: "/opt/tectonic".into(), guest: "/galley/bin/tectonic".into(), writable: false },
                Mount { host: "/cache".into(), guest: "/galley/cache".into(), writable: network },
            ],
            masked: vec!["/work/.git".into()],
            env: vec![("TECTONIC_CACHE_DIR".into(), "/galley/cache".into())],
            network,
            program: "/galley/bin/tectonic".into(),
            args: vec!["-X".into(), "compile".into(), "main.tex".into()],
            name: "galley-build-p1".into(),
        }
    }

    fn argv(cmd: &Command) -> Vec<String> {
        let std = cmd.as_std();
        std::iter::once(std.get_program().to_string_lossy().into_owned())
            .chain(std.get_args().map(|a| a.to_string_lossy().into_owned()))
            .collect()
    }

    #[test]
    fn guest_paths_map_only_when_isolated() {
        let sb = sandbox(SandboxKind::Docker);
        let m = sb.mounts(Path::new("/data/p1"), &spec(false).mounts);
        assert_eq!(m.guest(Path::new("/data/p1/.galley/build")), PathBuf::from("/work/.galley/build"));
        assert_eq!(m.guest(Path::new("/cache/x")), PathBuf::from("/galley/cache/x"));
        // A file mount maps to exactly its guest path, with no trailing separator.
        assert_eq!(m.guest(Path::new("/opt/tectonic")), PathBuf::from("/galley/bin/tectonic"));
        assert_eq!(m.guest(Path::new("/elsewhere")), PathBuf::from("/elsewhere"));
        let none = sandbox(SandboxKind::None).mounts(Path::new("/data/p1"), &[]);
        assert_eq!(none.guest(Path::new("/data/p1/main.tex")), PathBuf::from("/data/p1/main.tex"));
    }

    #[test]
    fn docker_isolates_network_and_filesystem() {
        let a = argv(&sandbox(SandboxKind::Docker).command(&spec(false)));
        let joined = a.join(" ");
        assert!(joined.contains("--network none"));
        assert!(joined.contains("-v /data/p1:/work:ro"));
        assert!(joined.contains("-v /data/p1/.galley/build:/work/.galley/build"));
        assert!(joined.contains("-v /cache:/galley/cache:ro"));
        assert!(joined.contains("--tmpfs /work/.git"));
        assert!(joined.contains("--cap-drop ALL"));
        assert!(joined.ends_with("debian:bookworm-slim /galley/bin/tectonic -X compile main.tex"));
        let fetch = argv(&sandbox(SandboxKind::Docker).command(&spec(true))).join(" ");
        assert!(fetch.contains("--network bridge"));
        assert!(fetch.contains("-v /cache:/galley/cache "));
        if Path::new("/etc/ssl/certs").is_dir() {
            assert!(fetch.contains("-v /etc/ssl/certs:/etc/ssl/certs:ro"));
            assert!(!joined.contains("/etc/ssl/certs"), "no certs without network");
        }
    }

    #[test]
    fn bwrap_unshares_everything_without_network() {
        let a = argv(&sandbox(SandboxKind::Bwrap).command(&spec(false))).join(" ");
        assert!(a.starts_with("bwrap "));
        assert!(a.contains("--ro-bind /data/p1 /work"));
        assert!(a.contains("--bind /data/p1/.galley/build /work/.galley/build"));
        assert!(a.contains("--unshare-net"));
        assert!(a.contains("--tmpfs /work/.git"));
        assert!(a.contains("--setenv TECTONIC_CACHE_DIR /galley/cache"));
        assert!(a.ends_with("-- /galley/bin/tectonic -X compile main.tex"));
        let net = argv(&sandbox(SandboxKind::Bwrap).command(&spec(true))).join(" ");
        assert!(!net.contains("--unshare-net"));
    }

    #[test]
    fn none_runs_directly_in_the_project() {
        let cmd = sandbox(SandboxKind::None).command(&spec(false));
        assert_eq!(argv(&cmd), vec!["/galley/bin/tectonic", "-X", "compile", "main.tex"]);
        assert_eq!(cmd.as_std().get_current_dir(), Some(Path::new("/data/p1")));
    }
}
