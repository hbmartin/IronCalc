#!/usr/bin/env bash
# Build strict Python docs without creating generated files in the checkout.
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
docs_scratch="$(mktemp -d "${TMPDIR:-/tmp}/ironcalc-python-docs.XXXXXX")"
trap 'rm -rf "$docs_scratch"' EXIT
cp -R "$repo_root/bindings/python/docs" "$docs_scratch/source"
cp "$repo_root/assets/logo/svg/orange+black.svg" "$docs_scratch/source/_static/logo-light.svg"
cp "$repo_root/assets/logo/svg/orange+white.svg" "$docs_scratch/source/_static/logo-dark.svg"
python -m sphinx -W --keep-going -b html "$docs_scratch/source" "$docs_scratch/html"
