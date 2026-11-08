#!/usr/bin/env bash
# Compile the packaged base tests without the surrounding repository.
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
package_scratch="$(mktemp -d "${TMPDIR:-/tmp}/ironcalc-base-package.XXXXXX")"
trap 'rm -rf "$package_scratch"' EXIT
cd "$repo_root"
cargo package --locked --allow-dirty --no-verify -p ironcalc_base --target-dir "$package_scratch/target"
package_archives=("$package_scratch/target/package"/ironcalc_base-*.crate)
test "${#package_archives[@]}" -eq 1
package_archive="${package_archives[0]}"
mkdir "$package_scratch/unpacked"
tar -xzf "$package_archive" -C "$package_scratch/unpacked"
package_name="${package_archive##*/}"
package_manifest="$package_scratch/unpacked/${package_name%.crate}/Cargo.toml"
test -f "$(dirname "$package_manifest")/tests/fixtures/invalid-iterations.ic"
cargo test --locked --manifest-path "$package_manifest" --lib --no-run
