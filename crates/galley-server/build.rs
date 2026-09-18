//! Guarantees `web/dist` exists for `rust-embed`, and builds the web app for release binaries so
//! `cargo build --release` yields a self-contained `galley`.

use std::path::Path;
use std::process::Command;

fn main() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web");
    let dist = web.join("dist");
    println!("cargo:rerun-if-changed={}", web.join("src").display());
    println!("cargo:rerun-if-changed={}", web.join("index.html").display());
    println!("cargo:rerun-if-changed={}", web.join("package.json").display());
    println!("cargo:rerun-if-env-changed=GALLEY_SKIP_WEB_BUILD");

    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let skip = std::env::var_os("GALLEY_SKIP_WEB_BUILD").is_some();
    if release && !skip && web.join("package.json").exists() {
        if !web.join("node_modules").exists() {
            run(&web, &["ci", "--no-audit", "--no-fund"]);
        }
        run(&web, &["run", "build"]);
    }
    std::fs::create_dir_all(&dist).expect("create web/dist");
}

fn run(web: &Path, args: &[&str]) {
    let status = Command::new("npm").args(args).current_dir(web).status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => panic!("npm {} failed with {s}. Fix the web build, or set GALLEY_SKIP_WEB_BUILD=1 to embed whatever is in web/dist.", args.join(" ")),
        Err(e) => panic!("could not run npm ({e}). Install Node.js 20+ to build the web app, or set GALLEY_SKIP_WEB_BUILD=1."),
    }
}
