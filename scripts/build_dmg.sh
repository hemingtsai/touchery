#!/bin/bash
# Build Touchery.app and package it into a DMG.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="Touchery"
BUNDLE_ID="com.touchery.app"
VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"

# Apple requires CFBundleShortVersionString to be one to three dot-separated
# integers and CFBundleVersion to be a numeric (dot-separated) build number.
# Cargo's semver — e.g. 1.2.0-beta — satisfies neither, so derive both from it
# while the artifact name keeps the full semver, beta suffix included.
SHORT_VERSION="$(python3 - "$VERSION" <<'PY'
import re, sys
match = re.match(r"^(\d+)\.(\d+)(?:\.(\d+))?", sys.argv[1])
if not match:
    sys.exit(f"cannot derive an Apple version from: {sys.argv[1]}")
print(".".join(part or "0" for part in match.groups()))
PY
)"
BUILD_NUMBER="$(python3 - "$SHORT_VERSION" <<'PY'
import sys
major, minor, patch = (int(part) for part in sys.argv[1].split("."))
print(major * 10000 + minor * 100 + patch)
PY
)"

# Respect the target directory Cargo actually uses (CARGO_TARGET_DIR et al.),
# rather than assuming the default `target/`.
TARGET_DIR="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"

STAGING="$TARGET_DIR/dmg"
APP_DIR="$STAGING/$APP_NAME.app"

echo "==> Building release binary"
# Ask Cargo where it put the executable instead of assuming target/release:
# with CARGO_TARGET_DIR or CARGO_BUILD_TARGET set that path is wrong or stale.
# --locked keeps the release reproducible and fails loudly if Cargo.lock is out
# of sync with the manifests.
BIN="$(cargo build --release --locked --message-format=json | python3 -c '
import json, sys
executable = ""
for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    message = json.loads(line)
    target = message.get("target", {})
    if message.get("reason") == "compiler-artifact" and message.get("executable") \
            and target.get("name") == "touchery":
        executable = message["executable"]
print(executable)
')"
if [ ! -x "$BIN" ]; then
    echo "error: cargo did not report a touchery executable ('$BIN')" >&2
    exit 1
fi

echo "==> Generating icon set"
ICONSET="$TARGET_DIR/touchery.iconset"
cargo run --release --locked --example gen_icon -- "$ICONSET" >/dev/null

echo "==> Assembling $APP_NAME.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>                 <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>          <string>$APP_NAME</string>
    <key>CFBundleIdentifier</key>           <string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key>              <string>$BUILD_NUMBER</string>
    <key>CFBundleShortVersionString</key>   <string>$SHORT_VERSION</string>
    <key>CFBundleExecutable</key>           <string>touchery</string>
    <key>CFBundlePackageType</key>          <string>APPL</string>
    <key>CFBundleIconFile</key>             <string>touchery</string>
    <key>CFBundleCategory</key>             <string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key>       <string>12.0</string>
    <key>NSHighResolutionCapable</key>      <true/>
    <!-- Menu bar (tray) app: no Dock icon, no main window at launch. -->
    <key>LSUIElement</key>                  <true/>
</dict>
</plist>
PLIST

cp "$BIN" "$APP_DIR/Contents/MacOS/touchery"
chmod +x "$APP_DIR/Contents/MacOS/touchery"

iconutil -c icns "$ICONSET" -o "$APP_DIR/Contents/Resources/touchery.icns"

# PlistBool sanity: ensure LSUIElement parsed as bool
plutil -lint "$APP_DIR/Contents/Info.plist"

echo "==> Ad-hoc codesigning"
codesign --force --deep -s - "$APP_DIR"

echo "==> Staging DMG contents (app + Applications symlink)"
ln -sfn /Applications "$STAGING/Applications"

echo "==> Creating DMG"
DMG="$TARGET_DIR/touchery-$VERSION.dmg"
rm -f "$DMG"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGING" -ov -format UDZO "$DMG"

echo "==> Done: $DMG"
ls -lh "$DMG"
