#!/usr/bin/env bash
# Assert the lockstep invariant from ADR-0023: the pinned toolchain channel in
# rust-toolchain.toml must equal the workspace MSRV (rust-version) in Cargo.toml.
# They're bumped together by hand, so this gate turns drift into a CI failure
# instead of a silently unverified MSRV claim. Pure text — no toolchain needed.
set -euo pipefail

pin=$(sed -n 's/^[[:space:]]*channel = "\(.*\)"/\1/p' rust-toolchain.toml)
msrv=$(sed -n 's/^[[:space:]]*rust-version = "\(.*\)"/\1/p' Cargo.toml)

if [ -z "$pin" ]; then
    echo "✗ could not read 'channel' from rust-toolchain.toml" >&2
    exit 1
fi
if [ -z "$msrv" ]; then
    echo "✗ could not read 'rust-version' from Cargo.toml [workspace.package]" >&2
    exit 1
fi

if [ "$pin" != "$msrv" ]; then
    echo "✗ MSRV lockstep broken (ADR-0023):" >&2
    echo "    rust-toolchain.toml channel = $pin" >&2
    echo "    Cargo.toml rust-version     = $msrv" >&2
    echo "  Set both to the same version (see CONTRIBUTING.md bump procedure)." >&2
    exit 1
fi

# Detached crates (own [workspace] table, issue #224) can't inherit the workspace
# rust-version, so they carry their own copy — hold each to the same pin.
for detached in crates/reuben-web/Cargo.toml; do
    dmsrv=$(sed -n 's/^[[:space:]]*rust-version = "\(.*\)"/\1/p' "$detached")
    if [ "$pin" != "$dmsrv" ]; then
        echo "✗ MSRV lockstep broken (ADR-0023) for detached crate $detached:" >&2
        echo "    rust-toolchain.toml channel = $pin" >&2
        echo "    $detached rust-version = ${dmsrv:-<missing>}" >&2
        exit 1
    fi
done

echo "✓ MSRV lockstep OK: $pin"
