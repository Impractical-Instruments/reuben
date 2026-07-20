# Why: The engine is headless — the SDK crate is the primary product and the CLI binary ships as versioned, installer-free CI release archives cut from a `v*` tag.

[Rule](../../web-product-process.md#versioned-release-archives)

reuben is an engine driven by something else, never a binary a non-technical person double-clicks, so
its packaging follows from that shape. The **crate is the primary product** — it is what an
in-process Rust consumer (and now the [product repo's submodule build](sdk-product-split.md)) uses;
build-from-source (`cargo build --release`) is the documented primary path, trivial for the Rust
audience. The standalone **CLI binary** is secondary: useful for out-of-process consumers and
standalone play. It ships as **versioned CI release archives** — a bare binary in a zip (Windows) or
tar.gz (Linux), the Rust-CLI convention (ripgrep, fd, bat) — triggered by a `v*` tag, which builds
`--release` on both platforms and attaches the archives.

**No installer**, deliberately: an MSI / Start-menu / PATH / uninstall flow buys nothing for a
headless CLI run from a terminal, and the prebuilt archive exists only as a convenience for non-Rust
Windows players whose real friction is the MSVC toolchain, not fetching a binary. The deeper claim
under all of this — the engine's product surface is its I/O contract, not pixels — is the same one
that keeps a dedicated UI out of the project ([sdk-product-split](sdk-product-split.md)); a headless
binary needs no GUI packaging because it was never going to have a GUI.

Distilled from: ADR-0026
