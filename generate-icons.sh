#!/usr/bin/env bash
# Generate square PNG icons (16..512 px) from factorio_chad.png.
# Used by build-appimage.sh; output goes to the given directory.
# Usage: ./generate-icons.sh [output-dir] [source-image]
set -euo pipefail

OUT_DIR="${1:-.build-assets}"
SRC="${2:-factorio_chad.png}"

MK="magick"
command -v magick >/dev/null 2>&1 || MK="convert"
command -v "$MK" >/dev/null 2>&1 || {
  echo "ERROR: ImageMagick not found. Install it: sudo apt install imagemagick" >&2
  exit 1
}
[ -f "$SRC" ] || { echo "ERROR: source image '$SRC' not found" >&2; exit 1; }

mkdir -p "$OUT_DIR/icons"

# Source art is 977x1024: shrink to fit a 1024 square canvas (centered),
# then scale to each size with high-quality filtering.
SQUARE="$OUT_DIR/icon-square-1024.png"
$MK "$SRC" -resize 1024x1024 -background none -gravity center -extent 1024x1024 "$SQUARE"

for SIZE in 16 24 32 48 64 128 256 512; do
  $MK "$SQUARE" -filter Lanczos -resize "${SIZE}x${SIZE}!" \
    "$OUT_DIR/icons/factorio-save-backup-manager-rust.png"
  $MK "$SQUARE" -filter Lanczos -resize "${SIZE}x${SIZE}!" \
    "$OUT_DIR/icons/${SIZE}x${SIZE}/apps/factorio-save-backup-manager-rust.png" 2>/dev/null || true
done

# Standard hicolor layout expected by linuxdeploy
rm -rf "$OUT_DIR/hicolor"
mkdir -p "$OUT_DIR/hicolor"
for SIZE in 16 24 32 48 64 128 256 512; do
  mkdir -p "$OUT_DIR/hicolor/${SIZE}x${SIZE}/apps"
  $MK "$SQUARE" -filter Lanczos -resize "${SIZE}x${SIZE}!" \
    "$OUT_DIR/hicolor/${SIZE}x${SIZE}/apps/factorio-save-backup-manager-rust.png"
done

echo "Icons written to $OUT_DIR/hicolor/"
