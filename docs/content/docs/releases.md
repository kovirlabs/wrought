+++
title = "Releases and verification"
description = "Prebuilt engine binaries for each preset, and how to check their checksums and signatures before running them."
weight = 51
+++

Each release has prebuilt `engine` binaries for Linux, one per preset and
processor:

```text
wrought-<version>-static-x86_64-unknown-linux-gnu.tar.gz
wrought-<version>-cms-x86_64-unknown-linux-gnu.tar.gz
wrought-<version>-app-x86_64-unknown-linux-gnu.tar.gz
wrought-<version>-<preset>-aarch64-unknown-linux-gnu.tar.gz   (Raspberry Pi 4/5, ARM servers)
SHA256SUMS
*.sig                                                         (one signature per file)
```

Each archive holds the `engine` binary, the licences, the README and
`SECURITY.md`. The binaries are built on Ubuntu 22.04 and need glibc 2.35 or
later. For another feature set, build from source
(`cargo build --release --features ...`).

## Verifying a download

Releases are signed with an SSH key (`ssh-keygen -Y sign`), so checking
them needs only OpenSSH 8.1 or later, which every current Linux and macOS
has. The public key is in
[`release/allowed_signers`](https://codeberg.org/kovirlabs/wrought/src/branch/main/release/allowed_signers)
in the repository; compare it with the copy on the project website before
you trust it the first time.

```sh
V=0.6.0 F=wrought-$V-cms-x86_64-unknown-linux-gnu.tar.gz
# 1. The signature of the checksum file, then the checksum of your archive.
ssh-keygen -Y verify -f allowed_signers -I wrought-release -n wrought-release \
  -s SHA256SUMS.sig < SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
# 2. Or check the archive's own signature directly.
ssh-keygen -Y verify -f allowed_signers -I wrought-release -n wrought-release \
  -s "$F.sig" < "$F"
```

A good signature prints `Good "wrought-release" signature for
wrought-release with ED25519 key SHA256:...`; anything else means the file
is not the one the project released. Do not run it.

## How releases are made

For contributors: a release is a tag `v<version>` matching the workspace
version in `Cargo.toml`. The release workflow
(`.github/workflows/release.yml`) runs `scripts/release.sh`, which builds
every preset for each target, writes reproducible archives (fixed file
order, owners and times), `SHA256SUMS`, and a signature for each file, then
verifies them against `release/allowed_signers`. The private key exists only
as the CI secret `RELEASE_SIGNING_KEY`.

To test the script locally without the real key:

```sh
scripts/release.sh --dry-run --presets static   # signs with a throwaway key
```

The output in `dist/` then carries a `DRY-RUN` file and the throwaway public
key; it must never be published.
