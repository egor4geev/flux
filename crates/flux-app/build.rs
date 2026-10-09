//! Builds the plugins bundled into Flux (stage 8, ADR-029) and embeds their files: every folder of
//! `plugins/` with a `flux-plugin.toml`. A plugin with code (a `Cargo.toml`) is built by cargo for
//! `wasm32-wasip2` into `target/plugins`, and its component is embedded under the path the
//! manifest's `wasm` names — the path it has when the same folder runs as a plugin under
//! development. The other files of the folder (the manifest, translations, icons) are embedded as
//! they are; sources and build files are not. `src/bundled.rs` includes the generated table.
//!
//! Without the target (`rustup target add wasm32-wasip2`), or when a plugin doesn't build, Flux is
//! built without that plugin, with a warning.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The target plugins are built for.
const TARGET: &str = "wasm32-wasip2";

fn main() {
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let workspace = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("crates/flux-app is two levels down")
        .to_path_buf();
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("set by cargo"));
    let plugins = workspace.join("plugins");
    // The whole folder: a new plugin, a changed source, translation or icon. Builds go elsewhere
    // (`target/plugins`), so a build doesn't trigger the next one.
    println!("cargo:rerun-if-changed={}", plugins.display());
    // A plugin is rebuilt when the API it is built against changes.
    for path in [
        "crates/flux-plugin-api/src",
        "crates/flux-plugin-api/Cargo.toml",
        "crates/flux-plugin/wit",
    ] {
        println!("cargo:rerun-if-changed={}", workspace.join(path).display());
    }

    let mut folders: Vec<PathBuf> = fs::read_dir(&plugins)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("flux-plugin.toml").is_file())
        .collect();
    folders.sort();

    let target_dir = workspace.join("target").join("plugins");
    let target_missing = !target_installed();
    let mut skipped = Vec::new();
    let mut table = String::from("&[\n");
    for folder in &folders {
        let name = folder.file_name().unwrap_or_default().to_string_lossy();
        let mut files = embedded_files(folder);
        if folder.join("Cargo.toml").is_file() {
            let component = if target_missing {
                Err(format!(
                    "the {TARGET} target is not installed (rustup target add {TARGET})"
                ))
            } else {
                build(folder, &target_dir, &out_dir, &name)
            };
            match component {
                Ok((path, wasm)) => files.push((path, wasm)),
                Err(reason) => {
                    println!(
                        "cargo:warning=Flux is built without the bundled plugin {name}: {reason}"
                    );
                    skipped.push(format!("{name}: {reason}"));
                    continue;
                }
            }
        }
        files.sort();
        writeln!(table, "    // plugins/{name}").unwrap();
        writeln!(
            table,
            "    flux_plugin::registry::Bundled {{\n        files: &["
        )
        .unwrap();
        for (relative, path) in files {
            writeln!(
                table,
                "            ({relative:?}, include_bytes!({:?})),",
                path.to_string_lossy()
            )
            .unwrap();
        }
        writeln!(table, "        ],\n    }},").unwrap();
    }
    table.push_str("]\n");
    fs::write(out_dir.join("bundled.rs"), table).expect("OUT_DIR is writable");
    // The tests of `bundled.rs` expect every plugin, unless one was left out.
    println!(
        "cargo:rustc-env=FLUX_BUNDLED_SKIPPED={}",
        skipped.join("; ")
    );
}

/// The files of a plugin's folder that go into Flux as they are, by their `/`-separated path in
/// the folder: everything but sources, build files and their output, and hidden files.
fn embedded_files(folder: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    let mut pending = vec![(folder.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = pending.pop() {
        for entry in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            let skip = name.starts_with('.')
                || (prefix.is_empty()
                    && matches!(
                        name.as_str(),
                        "target" | "src" | "Cargo.toml" | "Cargo.lock"
                    ));
            if skip {
                continue;
            }
            let relative = format!("{prefix}{name}");
            if path.is_dir() {
                pending.push((path, format!("{relative}/")));
            } else {
                files.push((relative, path));
            }
        }
    }
    files
}

/// Builds a plugin; returns its component's path in the folder (the manifest's `wasm`) and a copy
/// of the component in `OUT_DIR` (a `cargo clean -p` of the plugins doesn't break Flux's build).
fn build(
    folder: &Path,
    target_dir: &Path,
    out_dir: &Path,
    name: &str,
) -> Result<(String, PathBuf), String> {
    let manifest = fs::read_to_string(folder.join("flux-plugin.toml"))
        .map_err(|err| format!("flux-plugin.toml: {err}"))?;
    let wasm = manifest_wasm(&manifest)
        .ok_or("flux-plugin.toml names no `wasm` component, but there is a Cargo.toml")?;
    let file_name = Path::new(&wasm)
        .file_name()
        .ok_or_else(|| format!("`wasm = {wasm:?}` is not a file"))?;

    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command
        .args(["build", "--release", "--target", TARGET, "--target-dir"])
        .arg(target_dir)
        .current_dir(folder);
    // The plugin is a project of its own: nothing of this build (flags, wrappers such as
    // clippy-driver, the target, the jobserver) may leak into it.
    for (key, _) in env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        let leaks = (key.starts_with("CARGO_") && key != "CARGO_HOME")
            || matches!(
                key.as_str(),
                "RUSTFLAGS"
                    | "RUSTDOCFLAGS"
                    | "RUSTC_WRAPPER"
                    | "RUSTC_WORKSPACE_WRAPPER"
                    | "RUSTC_LINKER"
                    | "OUT_DIR"
                    | "TARGET"
                    | "HOST"
                    | "NUM_JOBS"
                    | "OPT_LEVEL"
                    | "DEBUG"
                    | "PROFILE"
                    | "MAKEFLAGS"
                    | "MFLAGS"
            );
        if leaks {
            command.env_remove(key);
        }
    }
    let output = command
        .output()
        .map_err(|err| format!("can't run cargo: {err}"))?;
    if !output.status.success() {
        let log = String::from_utf8_lossy(&output.stderr);
        let lines: Vec<&str> = log.lines().collect();
        let tail = lines[lines.len().saturating_sub(12)..].join(" | ");
        return Err(format!("cargo build failed: {tail}"));
    }
    let built = target_dir.join(TARGET).join("release").join(file_name);
    let copy = out_dir.join(format!("{name}.wasm"));
    fs::copy(&built, &copy).map_err(|err| format!("{}: {err}", built.display()))?;
    Ok((wasm, copy))
}

/// The manifest's top-level `wasm = "…"`, read without a TOML parser: the key comes before the
/// first table.
fn manifest_wasm(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some(value) = line
            .strip_prefix("wasm")
            .map(str::trim_start)
            .and_then(|rest| rest.strip_prefix('='))
        else {
            continue;
        };
        let value = value.trim();
        let value = value.strip_prefix('"')?;
        return Some(value[..value.find('"')?].to_string());
    }
    None
}

/// The standard library for the plugins' target is installed in the toolchain's sysroot.
fn target_installed() -> bool {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    Command::new(rustc)
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
                .join("lib/rustlib")
                .join(TARGET)
                .is_dir()
        })
        .unwrap_or(false)
}
