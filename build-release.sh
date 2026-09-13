#!/usr/bin/env bash
# Build a Windows release exe from Linux (see windows-build-spec.md §6.4).
# Usage: ./build-release.sh
set -euo pipefail

TARGET="x86_64-pc-windows-msvc"
BIN_NAME="factorio-save-backup-manager-rust"
DIST_DIR="dist"

# --- preflight checks -------------------------------------------------------
fail() { echo "ERROR: $1" >&2; exit 1; }

command -v cargo >/dev/null || fail "cargo not found on PATH"
command -v cargo-xwin >/dev/null || \
  fail "cargo-xwin not found. Install with: cargo install --locked cargo-xwin"
command -v llvm-rc >/dev/null || \
  fail "llvm-rc not found. Install LLVM: sudo apt install llvm"
rustup target list --installed | grep -q "$TARGET" || \
  fail "Rust target $TARGET missing. Install with: rustup target add $TARGET"

# --- version ----------------------------------------------------------------
VERSION=$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['packages'][0]['version'])") \
  || VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/[^0-9.]//g')
echo "==> Building $BIN_NAME v$VERSION for win64 (MSVC, cross-compiled)"

# --- build ------------------------------------------------------------------
cargo xwin build --release --target "$TARGET"

SRC="target/$TARGET/release/$BIN_NAME.exe"
[ -f "$SRC" ] || fail "Build output not found: $SRC"

# --- package ----------------------------------------------------------------
mkdir -p "$DIST_DIR"
OUT="$DIST_DIR/$BIN_NAME-$VERSION-win64.exe"
cp "$SRC" "$OUT"
sha256sum "$OUT" | tee "$OUT.sha256"

echo
echo "==> Done: $OUT"
