#!/usr/bin/env bash
# Package one release archive: copy each file or directory into a top-level `<name>/`, archive that
# as `dist/<name>.<ext>`, and write `dist/<name>.<ext>.sha256` beside it in `sha256sum`'s format, so
# `sha256sum -c` (or `shasum -a 256 -c`) checks it from `dist/`.
#
#   package-release.sh <name> <tar.gz|zip> <file-or-dir>...
#
# Runs on every release runner: Linux and macOS, and Windows under the Git Bash the runner ships,
# where `zip` is absent and 7-Zip is on the PATH. The archive is built inside the staging directory
# and moved out, so no archiver is ever handed an absolute path, which a native Windows program
# would read in MSYS form.
set -euo pipefail

if [ "$#" -lt 3 ]; then
    echo "usage: $0 <name> <tar.gz|zip> <file-or-dir>..." >&2
    exit 2
fi
name=$1
ext=$2
shift 2

# macOS tar otherwise writes an AppleDouble `._` file beside every entry that carries an xattr.
export COPYFILE_DISABLE=1

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
cp -R "$@" "$stage/$name/"

archive="$name.$ext"
case "$ext" in
    tar.gz)
        (cd "$stage" && tar -czf "$archive" "$name")
        ;;
    zip)
        if command -v 7z >/dev/null; then
            (cd "$stage" && 7z a -tzip -bso0 -bsp0 "$archive" "$name")
        else
            (cd "$stage" && zip -qr "$archive" "$name")
        fi
        ;;
    *)
        echo "unknown archive type: $ext" >&2
        exit 2
        ;;
esac

mkdir -p dist
mv "$stage/$archive" dist/
# The line is written here rather than taken from the tool: a Windows `sha256sum` marks the name
# with `*` (binary mode), and every sidecar should read the same.
if command -v sha256sum >/dev/null; then
    hash=$(sha256sum "dist/$archive")
else
    hash=$(shasum -a 256 "dist/$archive")
fi
printf '%s  %s\n' "${hash%% *}" "$archive" >"dist/$archive.sha256"
echo "dist/$archive"
