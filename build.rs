use std::{env, ffi::OsStr, fs, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    embed_resource::compile("assets/res.rc", embed_resource::NONE)
        .manifest_required()
        .expect("Failed to compile resource file");

    if env::var("CARGO_CFG_TARGET_OS").unwrap() == "windows" {
        // Include lib folder in the search path and place the dynamic runtime
        // beside the executable so locally built binaries are runnable.
        println!("cargo:rustc-link-search=lib");
        bundle_windows_runtime();
    }

    if let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        let is_dirty = Command::new("git")
            .args([
                "diff",
                "--ignore-matching-lines='^version = \".*\"'",
                "--quiet",
            ])
            .status()
            .unwrap()
            .code()
            .unwrap_or_default()
            != 0;

        let dirty = if is_dirty { "-dirty" } else { "" };
        let git_hash = String::from_utf8(output.stdout).unwrap();
        println!(
            "cargo:rustc-env=GIT_HASH={}{dirty}",
            git_hash.strip_suffix('\n').unwrap_or(&git_hash)
        );
    } else {
        println!("cargo:rustc-env=GIT_HASH=unknown-revision");
    }
}

fn bundle_windows_runtime() {
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set"));
    let runtime = manifest_dir.join("lib").join("SDL3.dll");
    println!("cargo:rerun-if-changed={}", runtime.display());

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is not set"));
    let profile_dir = out_dir
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(OsStr::new("build")))
        .and_then(|build_dir| build_dir.parent())
        .expect("OUT_DIR does not contain a Cargo profile build directory");
    let destination = profile_dir.join("SDL3.dll");

    fs::copy(&runtime, &destination).unwrap_or_else(|error| {
        panic!(
            "failed to copy {} to {}: {error}",
            runtime.display(),
            destination.display()
        )
    });
}
