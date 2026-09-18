//! Fetches the Tectonic static binary from its GitHub release on first use. The version is pinned,
//! so that every install compiles in the same way. Change the version only on purpose.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

pub const TECTONIC_VERSION: &str = "0.17.0";

pub fn tectonic_url() -> Result<String> {
    let triple = match std::env::consts::ARCH {
        "x86_64" => "x86_64-unknown-linux-musl",
        "aarch64" => "aarch64-unknown-linux-musl",
        other => {
            return Err(Error::Download(format!(
                "no Tectonic build for {other}. Install Tectonic yourself and set [build] tectonic_path in galley.toml."
            )))
        }
    };
    Ok(format!(
        "https://github.com/tectonic-typesetting/tectonic/releases/download/tectonic%40{v}/tectonic-{v}-{triple}.tar.gz",
        v = TECTONIC_VERSION
    ))
}

/// Download and unpack `tectonic` into `dir`. Returns the path of the binary. `progress` receives
/// short status lines for the build bar.
pub async fn install_tectonic(dir: &Path, progress: &(dyn Fn(String) + Send + Sync)) -> Result<PathBuf> {
    let url = tectonic_url()?;
    std::fs::create_dir_all(dir)?;
    progress(format!("Downloading Tectonic {TECTONIC_VERSION}…"));
    let client = reqwest::Client::builder()
        .user_agent(concat!("galley/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::Download(e.to_string()))?;
    let res = client
        .get(&url)
        .send()
        .await
        .map_err(|e| Error::Download(format!("{e}. Check the network, or install Tectonic yourself and set [build] tectonic_path.")))?;
    if !res.status().is_success() {
        return Err(Error::Download(format!("{url} answered {}", res.status())));
    }
    let bytes = res.bytes().await.map_err(|e| Error::Download(e.to_string()))?;
    progress("Unpacking Tectonic…".into());

    let dir = dir.to_path_buf();
    let binary = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let gz = flate2::read::GzDecoder::new(&bytes[..]);
        let mut archive = tar::Archive::new(gz);
        let target = dir.join("tectonic");
        let tmp = dir.join(".tectonic.download");
        let mut found = false;
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.to_path_buf();
            if path.file_name().and_then(|n| n.to_str()) == Some("tectonic") {
                entry.unpack(&tmp)?;
                found = true;
                break;
            }
        }
        if !found {
            return Err(Error::Download("the archive did not contain a tectonic binary".into()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&tmp, &target)?;
        Ok(target)
    })
    .await
    .map_err(|e| Error::Download(e.to_string()))??;
    progress("Tectonic ready".into());
    Ok(binary)
}
