#!/bin/sh
# Build the visual editor's bundle (editor/ -> crates/engine-admin/editor-dist/)
# from the pinned, locked npm packages. The bundle is committed, so building
# or running the engine never needs Node; this script is for whoever changes
# the editor, and for releases and CI, which rebuild it and fail when the
# committed bundle differs (so it is always the build of the committed
# sources).
#
#   scripts/build-editor.sh          # type-check, then build
#   scripts/build-editor.sh --check  # build into a temporary folder and
#                                    # compare with the committed bundle
#
# Needs Node.js 20+ and npm (network the first time: `npm ci` from
# editor/package-lock.json, install scripts off).
set -eu
cd "$(dirname "$0")/../editor"
DIST=../crates/engine-admin/editor-dist
command -v npm >/dev/null || { echo "build-editor: npm is needed" >&2; exit 2; }
if [ ! -d node_modules ] || [ package-lock.json -nt node_modules ]; then
  npm ci --no-audit --no-fund --ignore-scripts
fi
npx --no-install svelte-check --tsconfig ./tsconfig.json --fail-on-warnings
if [ "${1:-}" = "--check" ]; then
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT INT TERM
  npx --no-install vite build --logLevel warn --outDir "$TMP/dist"
  if ! diff -r "$TMP/dist" "$DIST" >/dev/null; then
    diff -r "$TMP/dist" "$DIST" | head -5 >&2
    echo "build-editor: the committed bundle in crates/engine-admin/editor-dist/ is not the build of editor/; run scripts/build-editor.sh and commit the result" >&2
    exit 1
  fi
  echo "build-editor: the committed bundle matches the sources"
else
  npx --no-install vite build --logLevel warn
  ls -l "$DIST"
fi
