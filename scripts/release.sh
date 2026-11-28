#!/bin/sh
# Build signed release archives of the `engine` binary for the presets
# (static, cms, app) and targets, with SHA-256 checksums and SSH signatures
# (`ssh-keygen -Y sign`, OpenSSH 8.1 or later; no other signing tool).
# Writes only to the output folder; never tags, uploads or publishes.
#
#   scripts/release.sh --dry-run                     # throwaway key, all presets
#   scripts/release.sh --dry-run --presets static    # quicker local test
#   HAMMERWRIGHT_RELEASE_TAG=v0.6.0 HAMMERWRIGHT_SIGNING_KEY="$(cat key)" \
#     scripts/release.sh                             # a real release (CI)
#
# Options:
#   --dry-run          sign with a key made for this run in a temporary
#                      folder and deleted afterwards; the output gets a
#                      DRY-RUN file and must never be published
#   --presets "..."    default "static cms app"
#   --targets "..."    default: HAMMERWRIGHT_RELEASE_TARGETS, else the host's
#                      target (x86_64-unknown-linux-gnu on the CI runner);
#                      aarch64-unknown-linux-gnu cross-compiles with
#                      aarch64-linux-gnu-gcc (Debian/Ubuntu package
#                      gcc-aarch64-linux-gnu)
#   --out DIR          default dist/ (git-ignored); emptied first
#
# Environment (never put the key in a file inside the repository):
#   HAMMERWRIGHT_SIGNING_KEY       the private key (OpenSSH format), e.g. from a
#                             CI secret; written to a 0600 file in a
#                             temporary folder for the run and removed after
#   HAMMERWRIGHT_SIGNING_KEY_FILE  or a path to the private key, or to a public
#                             key whose private half is in ssh-agent
#   HAMMERWRIGHT_ALLOWED_SIGNERS   allowed-signers file to verify the result with
#                             (default release/allowed_signers); a real run
#                             fails unless the key it signed with is listed
#   HAMMERWRIGHT_RELEASE_TAG       the pushed tag (CI); required for a real
#                             (signed) run and must be v<workspace version>
#
# Output, per target and preset: hammerwright-<version>-<preset>-<target>.tar.gz
# (engine, licences, README, SECURITY.md) and its .sig, plus SHA256SUMS and
# SHA256SUMS.sig. Users verify with (docs/content/docs/releases.md):
#   sha256sum -c --ignore-missing SHA256SUMS
#   ssh-keygen -Y verify -f allowed_signers -I hammerwright-release \
#     -n hammerwright-release -s FILE.sig < FILE
set -eu
cd "$(dirname "$0")/.."

NAMESPACE=hammerwright-release
PRINCIPAL=hammerwright-release
DRY_RUN=0
PRESETS="static cms app"
TARGETS="${HAMMERWRIGHT_RELEASE_TARGETS:-}"
OUT=dist

while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --presets) PRESETS="$2"; shift ;;
    --targets) TARGETS="$2"; shift ;;
    --out) OUT="$2"; shift ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

die() { echo "release: $*" >&2; exit 1; }

command -v ssh-keygen >/dev/null || die "ssh-keygen (OpenSSH 8.1+) is needed"
command -v sha256sum >/dev/null || die "sha256sum is needed"

VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[ -n "$VERSION" ] || die "cannot read the workspace version from Cargo.toml"
if [ -n "${HAMMERWRIGHT_RELEASE_TAG:-}" ] && [ "$HAMMERWRIGHT_RELEASE_TAG" != "v$VERSION" ]; then
  die "tag $HAMMERWRIGHT_RELEASE_TAG does not match the workspace version $VERSION"
fi

if [ -z "$TARGETS" ]; then
  TARGETS=$(rustc -vV | sed -n 's/^host: //p')
fi

for p in $PRESETS; do
  case "$p" in static|cms|app) ;; *) die "unknown preset: $p" ;; esac
done

# A temporary folder for the key; removed however the script ends.
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT INT TERM
umask 077

ALLOWED="${HAMMERWRIGHT_ALLOWED_SIGNERS:-release/allowed_signers}"
if [ "$DRY_RUN" = 1 ]; then
  KEY="$WORK/dry-run-key"
  ssh-keygen -q -t ed25519 -N "" -C "hammerwright dry-run $(date -u +%Y-%m-%dT%H:%M:%SZ)" -f "$KEY"
  ALLOWED="$WORK/allowed_signers"
  printf '%s namespaces="%s" %s\n' "$PRINCIPAL" "$NAMESPACE" "$(cut -d' ' -f1,2 "$KEY.pub")" > "$ALLOWED"
  echo "release: DRY RUN, signing with a throwaway key (deleted at exit)"
elif [ -z "${HAMMERWRIGHT_RELEASE_TAG:-}" ]; then
  # A real signature only for a tagged release: a branch or manual run
  # must never produce artefacts signed with the release key.
  die "a signed release needs HAMMERWRIGHT_RELEASE_TAG (the pushed v<version> tag); use --dry-run otherwise"
elif [ -n "${HAMMERWRIGHT_SIGNING_KEY:-}" ]; then
  KEY="$WORK/signing-key"
  printf '%s\n' "$HAMMERWRIGHT_SIGNING_KEY" > "$KEY"
  chmod 600 "$KEY"
elif [ -n "${HAMMERWRIGHT_SIGNING_KEY_FILE:-}" ]; then
  KEY="$HAMMERWRIGHT_SIGNING_KEY_FILE"
  [ -r "$KEY" ] || die "cannot read HAMMERWRIGHT_SIGNING_KEY_FILE"
else
  die "no signing key: set HAMMERWRIGHT_SIGNING_KEY or HAMMERWRIGHT_SIGNING_KEY_FILE, or use --dry-run"
fi
[ -r "$ALLOWED" ] || die "no allowed-signers file at $ALLOWED"
grep -q "^$PRINCIPAL " "$ALLOWED" || die "$ALLOWED lists no '$PRINCIPAL' key yet (see release/allowed_signers)"
umask 022

# The visual editor's bundle is committed (building the engine needs no
# Node); a release rebuilds it from the locked packages and refuses to
# ship when the committed one differs.
if command -v npm >/dev/null; then
  echo "== checking the visual editor bundle against editor/"
  scripts/build-editor.sh --check
elif [ "$DRY_RUN" = 1 ]; then
  echo "release: npm not found; skipping the editor bundle check (dry run only)"
else
  die "a release rebuilds the visual editor bundle to check it: install Node.js and npm"
fi

# Reproducible archives: fixed order, owners and times (the last commit's).
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2>/dev/null || date +%s)}"
export SOURCE_DATE_EPOCH

rm -rf "$OUT"
mkdir -p "$OUT"
STAGE="$WORK/stage"

for target in $TARGETS; do
  if command -v rustup >/dev/null; then
    rustup target add "$target" >/dev/null 2>&1 || true
  fi
  case "$target" in
    aarch64-unknown-linux-gnu)
      if [ "$(uname -m)" != aarch64 ]; then
        command -v aarch64-linux-gnu-gcc >/dev/null \
          || die "cross-compiling to $target needs aarch64-linux-gnu-gcc (package gcc-aarch64-linux-gnu)"
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER:-aarch64-linux-gnu-gcc}"
        export CC_aarch64_unknown_linux_gnu="${CC_aarch64_unknown_linux_gnu:-aarch64-linux-gnu-gcc}"
        export AR_aarch64_unknown_linux_gnu="${AR_aarch64_unknown_linux_gnu:-aarch64-linux-gnu-ar}"
      fi ;;
  esac
  for preset in $PRESETS; do
    echo "== building $preset for $target"
    if [ "$preset" = static ]; then
      cargo build --release --locked -p engine-cli --target "$target"
    else
      cargo build --release --locked -p engine-cli --target "$target" --features "$preset"
    fi
    name="hammerwright-$VERSION-$preset-$target"
    rm -rf "$STAGE"
    mkdir -p "$STAGE/$name"
    cp "target/$target/release/engine" "$STAGE/$name/engine"
    cp LICENSE-MIT LICENSE-APACHE README.md SECURITY.md "$STAGE/$name/"
    printf 'Hammerwright %s, preset %s, target %s\n' "$VERSION" "$preset" "$target" > "$STAGE/$name/BUILD.txt"
    find "$STAGE/$name" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
    tar -C "$STAGE" --sort=name --owner=0 --group=0 --numeric-owner \
      --mtime="@$SOURCE_DATE_EPOCH" -cf - "$name" | gzip -n -9 > "$OUT/$name.tar.gz"
  done
done

echo "== checksums and signatures"
(cd "$OUT" && sha256sum -- *.tar.gz > SHA256SUMS)
for f in "$OUT"/*.tar.gz "$OUT/SHA256SUMS"; do
  ssh-keygen -q -Y sign -f "$KEY" -n "$NAMESPACE" "$f"
done

echo "== verifying"
(cd "$OUT" && sha256sum -c --quiet SHA256SUMS)
for f in "$OUT"/*.tar.gz "$OUT/SHA256SUMS"; do
  ssh-keygen -Y verify -f "$ALLOWED" -I "$PRINCIPAL" -n "$NAMESPACE" -s "$f.sig" < "$f" >/dev/null \
    || die "signature check failed for $f"
done

if [ "$DRY_RUN" = 1 ]; then
  cat > "$OUT/DRY-RUN" <<EOF
These files were signed with a throwaway key that no longer exists.
They are for testing scripts/release.sh only. Never publish them.
EOF
  cp "$ALLOWED" "$OUT/allowed_signers.dry-run"
  chmod 644 "$OUT/allowed_signers.dry-run"
fi

echo "release: done, $(ls "$OUT" | wc -l) files in $OUT/"
ls -l "$OUT"
