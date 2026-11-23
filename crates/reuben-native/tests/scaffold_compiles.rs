//! `reuben scaffold-operator`'s output compiles in `reuben-core` as that crate ships: `no_std`.
//!
//! The crate is `#![cfg_attr(not(test), no_std)]`, so its lib *test* target links `std` and its
//! prelude. Text that leans on that prelude (a bare `Box`, a `std::` path) passes `cargo test -p
//! reuben-core` and fails `cargo build`. Only a non-test compile of the generated file can see
//! that, so this test runs the real CLI against a copy of `reuben-core` and lints the copy with
//! `cargo clippy -D warnings`, the same verdict the author's own gate reaches.
//!
//! The copy is a three-crate workspace (`reuben-core` plus the two path crates it depends on)
//! under `CARGO_TARGET_TMPDIR`, recopied from the real sources on every run. Its target dir
//! persists, so registry dependencies build once; the three copied crates rebuild each run.
//! `CARGO_TARGET_TMPDIR` is shared by every profile and by any worktree on the same target dir,
//! so a file lock serializes runs that would otherwise rewrite one copy under each other.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

const CRATES: [&str; 3] = ["reuben-contract", "reuben-macros", "reuben-core"];

/// One spec per template branch: a single signal output (the plain silence write), a
/// resource-bearing one with two signal outputs (the silence loop), and one with no signal
/// output whose ports cover every other `PortTy` arm, `i32` bounds at the ends of their range
/// included.
const SPECS: [(&str, &str); 3] = [
    (
        "scaffold_probe_signal",
        r#"{ "type_name": "scaffold_probe_signal",
             "inputs":  [{ "name": "in",  "ty": "f32_buffer" }],
             "outputs": [{ "name": "out", "ty": "f32_buffer" }] }"#,
    ),
    (
        "scaffold_probe_resources",
        r#"{ "type_name": "scaffold_probe_resources",
             "inputs":  [{ "name": "gate", "ty": "f32_buffer" }],
             "outputs": [{ "name": "left", "ty": "f32_buffer" }, { "name": "right", "ty": "f32_buffer" }],
             "resources": ["sample"] }"#,
    ),
    (
        "scaffold_probe_held",
        r#"{ "type_name": "scaffold_probe_held",
             "inputs": [
               { "name": "notes", "ty": "note" },
               { "name": "ctx", "ty": "harmony" },
               { "name": "rate", "ty": "f32",
                 "f32": { "min": 0.01, "max": 20.0, "default": 5.0, "unit": "Hz", "curve": "exponential" } },
               { "name": "mode", "ty": "enum", "vocab": "FilterMode" },
               { "name": "any", "ty": "arg" },
               { "name": "root", "ty": "pitch" },
               { "name": "pat", "ty": "str" },
               { "name": "offset", "ty": "i32",
                 "i32": { "min": -2147483648, "max": 2147483647, "default": 0 } },
               { "name": "level", "ty": "f32_buffer",
                 "f32": { "min": 0.0, "max": 1.0, "default": 0.5, "unit": "", "curve": "linear" } }
             ],
             "outputs": [{ "name": "pitch", "ty": "pitch" }, { "name": "chord", "ty": "harmony" }],
             "constants": [{ "name": "voices", "ty": "i32", "i32": { "min": 1, "max": 16, "default": 4 } }] }"#,
    ),
];

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// The real root manifest with its member lists swapped for the three copied crates. Everything
/// from `[workspace.package]` on (package metadata, the shared dependency table) is kept verbatim,
/// so the copy resolves exactly the dependency versions the workspace does.
fn copied_manifest(root_manifest: &str) -> String {
    let at = root_manifest
        .find("[workspace.package]")
        .expect("root Cargo.toml has a [workspace.package] table");
    let members: Vec<String> = CRATES.iter().map(|c| format!("\"crates/{c}\"")).collect();
    format!(
        "[workspace]\nresolver = \"2\"\nmembers = [{}]\n\n{}",
        members.join(", "),
        &root_manifest[at..]
    )
}

#[test]
fn scaffolded_operators_compile_in_no_std_reuben_core() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let base = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("scaffold-compiles");
    let ws = base.join("ws");
    fs::create_dir_all(&base).unwrap();
    let lock = File::create(base.join("lock")).unwrap();
    lock.lock().unwrap();

    for c in CRATES {
        let dest = ws.join("crates").join(c);
        let _ = fs::remove_dir_all(&dest);
        copy_dir(&repo.join("crates").join(c), &dest);
    }
    let root_manifest = fs::read_to_string(repo.join("Cargo.toml")).unwrap();
    fs::write(ws.join("Cargo.toml"), copied_manifest(&root_manifest)).unwrap();
    fs::copy(repo.join("Cargo.lock"), ws.join("Cargo.lock")).unwrap();
    fs::copy(
        repo.join("rust-toolchain.toml"),
        ws.join("rust-toolchain.toml"),
    )
    .unwrap();

    let core_root = ws.join("crates/reuben-core/src");
    for (name, spec) in SPECS {
        let spec_path = base.join(format!("{name}.json"));
        fs::write(&spec_path, spec).unwrap();
        // From inside the copy, so the CLI's own `cargo fmt -p reuben-core` formats the copy.
        let out = Command::new(env!("CARGO_BIN_EXE_reuben"))
            .current_dir(&ws)
            .arg("scaffold-operator")
            .arg("--spec")
            .arg(&spec_path)
            .arg("--core-root")
            .arg(&core_root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "scaffold-operator {name} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // `--lib` is the shipped, `no_std` shape this test exists for; `--tests` also lints the
    // generated test module, which only the `std` test target compiles.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(&ws)
        .args([
            "clippy",
            "--offline",
            "-p",
            "reuben-core",
            "--lib",
            "--tests",
            "--",
            "-D",
            "warnings",
        ])
        .env("CARGO_TARGET_DIR", base.join("target"))
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "scaffolded operators fail clippy in reuben-core:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
