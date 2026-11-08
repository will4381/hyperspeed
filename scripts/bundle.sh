#!/bin/sh
# Installs Hyperspeed.app into /Applications and the `hyperspeed` CLI into ~/.cargo/bin.
set -e
cd "$(dirname "$0")/.."

export HYPERSPEED_COMMIT="$(git rev-parse HEAD 2>/dev/null || true)"
cargo install --path . --force
BIN="$HOME/.cargo/bin/hyperspeed"
APP="/Applications/Hyperspeed.app"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp assets/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
cp "$BIN" "$APP/Contents/MacOS/hyperspeed"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Hyperspeed</string>
  <key>CFBundleIdentifier</key><string>com.relative.hyperspeed</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleExecutable</key><string>hyperspeed</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
PLIST
codesign --force --deep --sign - "$APP"
echo "Installed $APP"
