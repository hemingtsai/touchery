#!/bin/bash
# Build Touchery.app and package it into a DMG.
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="Touchery"
BUNDLE_ID="com.touchery.app"
VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')"
STAGING="target/dmg"
APP_DIR="$STAGING/$APP_NAME.app"

echo "==> Building release binary"
cargo build --release

echo "==> Generating icon set"
ICONSET="target/touchery.iconset"
cargo run --release --example gen_icon -- "$ICONSET" >/dev/null

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
    <key>CFBundleVersion</key>              <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>   <string>$VERSION</string>
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

cp target/release/touchery "$APP_DIR/Contents/MacOS/touchery"
chmod +x "$APP_DIR/Contents/MacOS/touchery"

iconutil -c icns "$ICONSET" -o "$APP_DIR/Contents/Resources/touchery.icns"

# PlistBool sanity: ensure LSUIElement parsed as bool
plutil -lint "$APP_DIR/Contents/Info.plist"

echo "==> Ad-hoc codesigning"
codesign --force --deep -s - "$APP_DIR"

echo "==> Staging DMG contents (app + Applications symlink)"
ln -sfn /Applications "$STAGING/Applications"

echo "==> Creating DMG"
DMG="target/touchery-$VERSION.dmg"
rm -f "$DMG"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGING" -ov -format UDZO "$DMG"

echo "==> Done: $DMG"
ls -lh "$DMG"
