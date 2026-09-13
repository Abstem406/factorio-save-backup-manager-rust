#!/usr/bin/env bash
# Build a Linux AppImage from the local system. The AppImage bundles shared
# libraries (OpenSSL, fontconfig, freetype...) so it runs on any modern distro.
# Usage: ./release-linux.sh
set -euo pipefail

BIN_NAME="factorio-save-backup-manager-rust"
DIST_DIR="dist"
BUILD_DIR=".appimage-build"
APPDIR="$BUILD_DIR/AppDir"

fail() { echo "ERROR: $1" >&2; exit 1; }

command -v cargo >/dev/null || fail "cargo not found on PATH"
command -v python3 >/dev/null || fail "python3 not found on PATH"
[ -f factorio_chad.png ] || fail "factorio_chad.png not found in project root"

VERSION=$(cargo metadata --no-deps --format-version 1 2>/dev/null \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['packages'][0]['version'])")

echo "==> Building $BIN_NAME v$VERSION (Linux x86_64)"

# --- 1. compile --------------------------------------------------------------
cargo build --release
BIN="target/release/$BIN_NAME"
[ -f "$BIN" ] || fail "Build output not found: $BIN"

# --- 2. AppDir layout --------------------------------------------------------
rm -rf "$BUILD_DIR"
mkdir -p "$APPDIR/usr/bin"

cp "$BIN" "$APPDIR/usr/bin/"

./generate-icons.sh "$BUILD_DIR/assets"
cp -r "$BUILD_DIR/assets/hicolor" "$APPDIR/usr/"

cat > "$APPDIR/$BIN_NAME.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Factorio Save Backup Manager
GenericName=Backup Manager
Comment=Backup and restore Factorio saves to Google Drive / Discord
Exec=$BIN_NAME
Icon=factorio-save-backup-manager-rust
Categories=Game;Utility;
Terminal=false
StartupWMClass=factorio-save-backup-manager-rust
EOF

# Metadata file for linuxdeploy / appstreamcli.
# NOTE: filename must match the component <id> (avoids appstream warnings).
METAINFO_ID="com.github.abstem406.FactorioSaveBackupManager"
mkdir -p "$APPDIR/usr/share/metainfo"
cat > "$APPDIR/usr/share/metainfo/$METAINFO_ID.appdata.xml" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>$METAINFO_ID</id>
  <metadata_license>MIT</metadata_license>
  <project_license>LicenseRef-Proprietary</project_license>
  <name>Factorio Save Backup Manager</name>
  <developer_name>Abstem406</developer_name>
  <summary>Backup and restore Factorio saves to Google Drive / Discord</summary>
  <url type="homepage">https://github.com/Abstem406/factorio-save-backup-manager-rust</url>
  <description>
    <p>Desktop app to keep Factorio saves safe: upload them to Google Drive,
    get notified on Discord, and restore or share backups with a couple of
    clicks.</p>
  </description>
  <launchable type="desktop-id">$BIN_NAME.desktop</launchable>
  <releases>
    <release version="$VERSION" date="$(date +%F)"/>
  </releases>
  <content_rating type="oars-1.1"/>
</component>
EOF

# --- 3. linuxdeploy (bundles libs, generates AppRun) -------------------------
LINUXDEPLOY="$BUILD_DIR/linuxdeploy-x86_64.AppImage"
if [ ! -f "$LINUXDEPLOY" ]; then
  echo "==> Downloading linuxdeploy"
  curl -sL -o "$LINUXDEPLOY" \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-x86_64.AppImage"
  chmod +x "$LINUXDEPLOY"
fi

echo "==> Running linuxdeploy (bundles shared libraries)"
chmod +x "$APPDIR/usr/bin/$BIN_NAME"
OUTPUT="$DIST_DIR/$BIN_NAME-$VERSION-x86_64.AppImage" \
  "$LINUXDEPLOY" \
  --appdir "$APPDIR" \
  --desktop-file "$APPDIR/$BIN_NAME.desktop" \
  --icon-file "$BUILD_DIR/assets/hicolor/256x256/apps/$BIN_NAME.png" \
  --output appimage

mkdir -p "$DIST_DIR"
ARTIFACT="$DIST_DIR/$BIN_NAME-$VERSION-x86_64.AppImage"
[ -f "$ARTIFACT" ] || fail "AppImage was not created (linuxdeploy failed)"
sha256sum "$ARTIFACT" | tee "$ARTIFACT.sha256"

echo
echo "==> Done: $ARTIFACT"
echo "    Test with: $ARTIFACT"
