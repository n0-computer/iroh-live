//! Builds the web client that the relay embeds.

use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Installs the web client's dependencies and bundles it into `web/dist`.
fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ui_dir = manifest_dir.join("web");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/package-lock.json");
    // Only files that exist: cargo reruns the script on every build while a
    // listed file is missing.
    println!("cargo:rerun-if-changed=web/vite.config.ts");
    println!("cargo:rerun-if-changed=web/src");
    if needs_install(&ui_dir) {
        run_npm(&ui_dir, &["install"]);
    }
    run_npm(&ui_dir, &["run", "build"]);
}

/// Reports whether `node_modules` is missing or older than the manifest or the lockfile.
///
/// npm records the tree it installed in `node_modules/.package-lock.json`.
/// That file is older than `package.json` or `package-lock.json` only when
/// one of them changed since. The lockfile is not checked in, so a missing
/// one also needs an install.
fn needs_install(ui_dir: &Path) -> bool {
    let modified =
        |name: &str| std::fs::metadata(ui_dir.join(name)).and_then(|meta| meta.modified());
    let Ok(installed) = modified("node_modules/.package-lock.json") else {
        return true;
    };
    ["package.json", "package-lock.json"]
        .into_iter()
        .any(|name| match modified(name) {
            Ok(changed) => changed > installed,
            Err(_) => true,
        })
}

/// Runs `npm` with `args` in `ui_dir`, and panics if it fails.
fn run_npm(ui_dir: &Path, args: &[&str]) {
    let status = Command::new("npm")
        .args(args)
        .current_dir(ui_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .unwrap_or_else(|err| panic!("failed to run npm {}: {err}", args.join(" ")));
    if !status.success() {
        panic!("npm {} failed with status {status}", args.join(" "));
    }
}
