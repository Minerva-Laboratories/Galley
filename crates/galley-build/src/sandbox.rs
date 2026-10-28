//! One way to run a child process, with three backends. The project directory is mounted read-only
//! at `/work`, with `.git` and `.galley` hidden. A compile writes only into the scratch folders
//! mounted for it (see [`crate::zones`]).

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
    /// Mounted in order, after `masked`, so a mount can show one folder inside a masked one.
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
            if host.starts_with(h)
                && best.is_none_or(|(bh, _)| h.components().count() > bh.components().count())
            {
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

impl Spec {
    /// The whole environment of a compile. The server's own environment holds its secrets (the
    /// worker token, backup keys, the OpenID Connect client secret), and a document can read
    /// environment variables, so nothing is inherited: only what the engine set, plus a search path
    /// and a locale when the engine did not set them.
    fn environment(&self, isolated: bool) -> Vec<(String, String)> {
        let mut env = self.env.clone();
        let has = |env: &[(String, String)], key: &str| env.iter().any(|(k, _)| k == key);
        if !has(&env, "PATH") {
            let path = std::env::var("PATH")
                .unwrap_or_else(|_| "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into());
            env.push(("PATH".into(), path));
        }
        if isolated {
            if !has(&env, "LANG") {
                env.push(("LANG".into(), "C.UTF-8".into()));
            }
        } else {
            // Without a sandbox the compile runs on this machine, whose locale names may differ.
            for key in ["LANG", "LC_ALL", "LC_CTYPE", "TMPDIR"] {
                if let (false, Ok(value)) = (has(&env, key), std::env::var(key)) {
                    env.push((key.into(), value));
                }
            }
        }
        env
    }

    /// Refuse to start when a mount could reach outside what Galley meant to share. A writable
    /// mount must be one of Galley's scratch folders. Bind mounts follow links, so no source
    /// inside a project (anything below the folder holding `.galley`, or below the project
    /// mounted at /work) may be or pass through a link. System sources such as the TeX tree or
    /// the Tectonic binary are the server's own and may be links.
    pub fn check_mounts(&self) -> std::io::Result<()> {
        let refuse = |why: String| Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, why));
        for m in &self.mounts {
            if m.writable && !crate::zones::is_scratch(&m.host) {
                return refuse(format!("{} is not a scratch folder", m.host.display()));
            }
            let project = m
                .host
                .ancestors()
                .find(|a| a.file_name().is_some_and(|n| n == ".galley"))
                .and_then(Path::parent)
                .or_else(|| m.host.starts_with(&self.project_dir).then_some(self.project_dir.as_path()));
            let Some(project) = project else { continue };
            let Ok(rest) = m.host.strip_prefix(project) else { continue };
            let mut at = project.to_path_buf();
            for part in rest.components() {
                at.push(part);
                if std::fs::symlink_metadata(&at)?.file_type().is_symlink() {
                    return refuse(format!("{} is a link", at.display()));
                }
            }
        }
        Ok(())
    }
}

impl Sandbox {
    /// Resolve `auto` with a probe, in the order of the spec: bwrap, then docker, then none.
    pub async fn detect(
        preference: &str,
        docker_image: &str,
        memory_mb: u64,
        cpus: f32,
    ) -> Sandbox {
        let systemd_scope = probe(
            "systemd-run",
            &[
                "--user",
                "--scope",
                "-q",
                "-p",
                "MemoryMax=64M",
                "--",
                "/usr/bin/true",
            ],
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
                c.args(&spec.args).current_dir(&spec.project_dir).env_clear();
                for (k, v) in spec.environment(false) {
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
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);
        }
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
        for dir in [
            "/usr",
            "/lib",
            "/lib64",
            "/bin",
            "/sbin",
            "/etc/alternatives",
        ] {
            if Path::new(dir).exists() {
                argv.extend(["--ro-bind", dir, dir].map(String::from));
            }
        }
        // The CA bundle is mounted even without network, read-only. Tectonic may build an HTTPS
        // client when it starts, and a client with no roots panics before it reports which file
        // it wanted, which would hide a missing package behind a crash.
        for f in [
            "/etc/ssl",
            "/etc/ca-certificates",
            "/etc/texmf",
            "/etc/fonts",
            "/var/lib/texmf",
            "/var/cache/fontconfig",
        ] {
            if Path::new(f).exists() {
                argv.extend(["--ro-bind", f, f].map(String::from));
            }
        }
        if spec.network {
            for f in ["/etc/resolv.conf", "/etc/hosts"] {
                if Path::new(f).exists() {
                    argv.extend(["--ro-bind", f, f].map(String::from));
                }
            }
        }
        argv.extend(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"].map(String::from));
        argv.extend(
            ["--ro-bind", &spec.project_dir.to_string_lossy(), GUEST_WORK].map(String::from),
        );
        for p in &spec.masked {
            argv.extend(["--tmpfs", &p.to_string_lossy()].map(String::from));
        }
        for m in &spec.mounts {
            let flag = if m.writable { "--bind" } else { "--ro-bind" };
            argv.extend(
                [flag, &m.host.to_string_lossy(), &m.guest.to_string_lossy()].map(String::from),
            );
        }
        argv.extend(
            [
                "--unshare-user",
                "--unshare-pid",
                "--unshare-ipc",
                "--unshare-uts",
                "--unshare-cgroup",
            ]
            .map(String::from),
        );
        if !spec.network {
            argv.push("--unshare-net".into());
        }
        argv.extend(
            ["--die-with-parent", "--new-session", "--chdir", GUEST_WORK].map(String::from),
        );
        // `--clearenv` drops everything bwrap inherited, so it comes before the settings below.
        argv.push("--clearenv".into());
        for (k, v) in spec.environment(true) {
            argv.extend(["--setenv".into(), k, v]);
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
        c.args([
            "--memory",
            &format!("{}m", self.memory_mb),
            "--cpus",
            &format!("{}", self.cpus),
        ]);
        c.args([
            "--pids-limit",
            "512",
            "--security-opt",
            "no-new-privileges",
            "--cap-drop",
            "ALL",
        ]);
        c.args(["--read-only", "--tmpfs", "/tmp:rw,exec,size=512m"]);
        c.args(["--user", &format!("{}:{}", uid(), gid())]);
        // A slim image has no CA bundle, so mount the host's, read-only. Compiles are offline, but
        // Tectonic panics when it builds an HTTPS client with no roots, which would hide a missing
        // package behind a crash.
        for certs in ["/etc/ssl/certs", "/etc/ca-certificates"] {
            if Path::new(certs).is_dir() {
                c.args(["-v", &format!("{certs}:{certs}:ro")]);
            }
        }
        c.args([
            "-v",
            &format!("{}:{GUEST_WORK}:ro", spec.project_dir.display()),
        ]);
        for p in &spec.masked {
            c.args(["--tmpfs", &p.to_string_lossy()]);
        }
        for m in &spec.mounts {
            let ro = if m.writable { "" } else { ":ro" };
            c.args([
                "-v",
                &format!("{}:{}{ro}", m.host.display(), m.guest.display()),
            ]);
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
    /// killed. `docker rm -f` returns once the container is gone, so it no longer writes anywhere.
    pub async fn kill(&self, spec: &Spec) {
        if self.kind == SandboxKind::Docker {
            let _ = Command::new("docker")
                .args(["rm", "-f", &spec.name])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }

    /// Something that removes the run's docker container if the run is abandoned: a request that
    /// went away drops the future that waits for the container, and the container would keep
    /// running, still writing to its scratch folder.
    pub fn guard(&self, spec: &Spec) -> ContainerGuard {
        ContainerGuard((self.kind == SandboxKind::Docker).then(|| spec.name.clone()))
    }

    /// A timed-out command may have spawned latexmk, bibliography tools, and converters.
    /// Commands are started in their own process group, so kill every host-side descendant.
    pub async fn kill_group(&self, pid: Option<u32>) {
        #[cfg(unix)]
        if let Some(pid) = pid.filter(|pid| *pid > 1) {
            let _ = Command::new("kill")
                .arg("-KILL")
                .arg("--")
                .arg(format!("-{pid}"))
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        }
    }
}

/// See [`Sandbox::guard`].
pub struct ContainerGuard(Option<String>);

impl ContainerGuard {
    /// The run ended on its own, so there is nothing left to remove.
    pub fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        if let Some(name) = self.0.take() {
            std::thread::spawn(move || {
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", &name])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            });
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
    std::fs::metadata("/proc/self")
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.uid()
        })
        .unwrap_or(1000)
}

fn gid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|m| {
            use std::os::unix::fs::MetadataExt;
            m.gid()
        })
        .unwrap_or(1000)
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
            mounts: vec![
                Mount {
                    host: "/data/p1/.galley/scratch/run-1".into(),
                    guest: "/tmp/galley-out".into(),
                    writable: true,
                },
                Mount {
                    host: "/opt/tectonic".into(),
                    guest: "/galley/bin/tectonic".into(),
                    writable: false,
                },
                Mount {
                    host: "/cache".into(),
                    guest: "/galley/cache".into(),
                    writable: network,
                },
            ],
            masked: vec!["/work/.git".into(), "/work/.galley".into()],
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
        assert_eq!(
            m.guest(Path::new("/data/p1/.galley/scratch/run-1/main.pdf")),
            PathBuf::from("/tmp/galley-out/main.pdf")
        );
        assert_eq!(
            m.guest(Path::new("/cache/x")),
            PathBuf::from("/galley/cache/x")
        );
        // A file mount maps to exactly its guest path, with no trailing separator.
        assert_eq!(
            m.guest(Path::new("/opt/tectonic")),
            PathBuf::from("/galley/bin/tectonic")
        );
        assert_eq!(
            m.guest(Path::new("/elsewhere")),
            PathBuf::from("/elsewhere")
        );
        let none = sandbox(SandboxKind::None).mounts(Path::new("/data/p1"), &[]);
        assert_eq!(
            none.guest(Path::new("/data/p1/main.tex")),
            PathBuf::from("/data/p1/main.tex")
        );
    }

    #[test]
    fn docker_isolates_network_and_filesystem() {
        let a = argv(&sandbox(SandboxKind::Docker).command(&spec(false)));
        let joined = a.join(" ");
        assert!(joined.contains("--network none"));
        assert!(joined.contains("-v /data/p1:/work:ro"));
        assert!(joined.contains("-v /data/p1/.galley/scratch/run-1:/tmp/galley-out "));
        assert!(!joined.contains(".galley/build"));
        assert!(joined.contains("--tmpfs /work/.galley"));
        assert!(joined.contains("-v /cache:/galley/cache:ro"));
        assert!(joined.contains("--tmpfs /work/.git"));
        assert!(joined.contains("--cap-drop ALL"));
        assert!(joined.ends_with("debian:bookworm-slim /galley/bin/tectonic -X compile main.tex"));
        let fetch = argv(&sandbox(SandboxKind::Docker).command(&spec(true))).join(" ");
        assert!(fetch.contains("--network bridge"));
        assert!(fetch.contains("-v /cache:/galley/cache "));
        if Path::new("/etc/ssl/certs").is_dir() {
            assert!(fetch.contains("-v /etc/ssl/certs:/etc/ssl/certs:ro"));
            // Certs travel with both passes. Without them Tectonic panics instead of naming the
            // file it could not find, and the fetch pass never runs.
            assert!(joined.contains("-v /etc/ssl/certs:/etc/ssl/certs:ro"));
        }
    }

    #[test]
    fn bwrap_unshares_everything_without_network() {
        let a = argv(&sandbox(SandboxKind::Bwrap).command(&spec(false))).join(" ");
        assert!(a.starts_with("bwrap "));
        assert!(a.contains("--ro-bind /data/p1 /work"));
        assert!(a.contains("--bind /data/p1/.galley/scratch/run-1 /tmp/galley-out"));
        assert!(!a.contains(".galley/build"));
        assert!(a.contains("--unshare-net"));
        assert!(a.contains("--tmpfs /work/.git"));
        assert!(a.contains("--setenv TECTONIC_CACHE_DIR /galley/cache"));
        assert!(a.ends_with("-- /galley/bin/tectonic -X compile main.tex"));
        let net = argv(&sandbox(SandboxKind::Bwrap).command(&spec(true))).join(" ");
        assert!(!net.contains("--unshare-net"));
    }

    #[test]
    fn compiles_inherit_no_environment() {
        let a = argv(&sandbox(SandboxKind::Bwrap).command(&spec(false)));
        let clear = a.iter().position(|x| x == "--clearenv").expect("--clearenv");
        let first_set = a.iter().position(|x| x == "--setenv").expect("--setenv");
        assert!(clear < first_set, "--clearenv must come before the settings");
        let joined = a.join(" ");
        assert!(joined.contains("--setenv LANG C.UTF-8"));
        assert!(joined.contains("--setenv PATH "));
        // A secret in the server's environment is never named on the command line either.
        assert!(!joined.contains("GALLEY_WORKER_TOKEN"));

        let none = sandbox(SandboxKind::None).command(&spec(false));
        let set: Vec<String> = none
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| v.map(|_| k.to_string_lossy().into_owned()))
            .collect();
        assert!(set.contains(&"TECTONIC_CACHE_DIR".to_string()));
        assert!(set.contains(&"PATH".to_string()));
        assert!(!set.iter().any(|k| k.starts_with("GALLEY_") || k.starts_with("AWS_")), "{set:?}");
    }

    #[test]
    fn masks_come_before_mounts_so_a_mount_can_show_through_one() {
        let mut s = spec(false);
        s.mounts.push(Mount {
            host: "/data/p1/.galley/svg".into(),
            guest: "/work/.galley/svg".into(),
            writable: false,
        });
        let a = argv(&sandbox(SandboxKind::Bwrap).command(&s)).join(" ");
        let mask = a.find("--tmpfs /work/.galley").unwrap();
        let svg = a.find("--ro-bind /data/p1/.galley/svg /work/.galley/svg").unwrap();
        assert!(mask < svg, "{a}");
    }

    #[cfg(unix)]
    #[test]
    fn only_scratch_folders_are_writable_and_no_source_is_a_link() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p1");
        std::fs::create_dir(&project).unwrap();
        crate::zones::prepare(&project).unwrap();
        let leaf = crate::zones::Leaf::run(&project).unwrap();
        let ok = |host: PathBuf, writable: bool| Spec {
            project_dir: project.clone(),
            mounts: vec![Mount { host, guest: "/tmp/galley-out".into(), writable }],
            ..spec(false)
        };
        assert!(ok(leaf.path().to_path_buf(), true).check_mounts().is_ok());
        // Galley's own folder is never writable by a compile.
        assert!(ok(crate::zones::build_dir(&project), true).check_mounts().is_err());
        assert!(ok(crate::zones::inputs_dir(&project), true).check_mounts().is_err());
        assert!(ok(crate::zones::inputs_dir(&project), false).check_mounts().is_ok());
        // A source that is a link, or that sits below one, is refused even read-only.
        let elsewhere = dir.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("svg")).unwrap();
        symlink(&elsewhere, project.join(".galley/svg")).unwrap();
        assert!(ok(project.join(".galley/svg"), false).check_mounts().is_err());
        std::fs::remove_file(project.join(".galley/svg")).unwrap();
        symlink(&elsewhere, project.join("figs")).unwrap();
        assert!(ok(project.join("figs/svg"), false).check_mounts().is_err());
        // A system source outside the project may be a link (a linked TeX tree or binary).
        let tool = dir.path().join("bin-latexdiff");
        symlink(elsewhere.join("svg"), &tool).unwrap();
        assert!(ok(tool, false).check_mounts().is_ok());
    }

    #[test]
    fn none_runs_directly_in_the_project() {
        let cmd = sandbox(SandboxKind::None).command(&spec(false));
        assert_eq!(
            argv(&cmd),
            vec!["/galley/bin/tectonic", "-X", "compile", "main.tex"]
        );
        assert_eq!(cmd.as_std().get_current_dir(), Some(Path::new("/data/p1")));
    }
}
