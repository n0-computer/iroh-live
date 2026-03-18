use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ui_dir = manifest_dir.parent().expect("workspace root").join("iroh-live-relay/web");
    println!("cargo:rerun-if-changed=web/package.json");
    println!("cargo:rerun-if-changed=web/package-lock.json");
    println!("cargo:rerun-if-changed=web/vite.config.ts");
    println!("cargo:rerun-if-changed=web/tsconfig.json");
    println!("cargo:rerun-if-changed=web/tsconfig.node.json");
    println!("cargo:rerun-if-changed=web/index.html");
    println!("cargo:rerun-if-changed=web/src");
    run_npm(&ui_dir, &["install"]);
    run_npm(&ui_dir, &["run", "build"]);
}

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
