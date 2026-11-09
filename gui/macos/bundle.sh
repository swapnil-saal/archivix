#!/usr/bin/env bash
# Build archivix.app and a drag-to-Applications .dmg using only tools that ship
# with macOS and Xcode CLT (iconutil, codesign, hdiutil, lipo).
#
#   gui/macos/bundle.sh              # native architecture
#   UNIVERSAL=1 gui/macos/bundle.sh  # arm64 + x86_64 universal binary
#
# Output: dist/archivix.app and dist/archivix-<version>-macos.dmg
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"
VERSION="$(grep -m1 '^version' gui/Cargo.toml | cut -d'"' -f2)"
OUT="${OUT:-$ROOT/dist}"
APP="$OUT/archivix.app"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

if [[ "${UNIVERSAL:-0}" == "1" ]]; then
  for t in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release --locked -p archivix-gui --target "$t"
  done
  lipo -create -output "$WORK/archivix-gui" \
    target/aarch64-apple-darwin/release/archivix-gui \
    target/x86_64-apple-darwin/release/archivix-gui
else
  cargo build --release --locked -p archivix-gui
  cp target/release/archivix-gui "$WORK/archivix-gui"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$WORK/archivix-gui" "$APP/Contents/MacOS/archivix-gui"

# Icon: the app renders its own iconset; iconutil packs it into .icns.
"$WORK/archivix-gui" --write-iconset "$WORK/archivix.iconset"
iconutil -c icns -o "$APP/Contents/Resources/archivix.icns" "$WORK/archivix.iconset"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>archivix</string>
  <key>CFBundleDisplayName</key><string>archivix</string>
  <key>CFBundleIdentifier</key><string>io.github.swapnil-saal.archivix</string>
  <key>CFBundleExecutable</key><string>archivix-gui</string>
  <key>CFBundleIconFile</key><string>archivix</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>MIT OR Apache-2.0</string>
</dict>
</plist>
PLIST

# Ad-hoc signature (required to run on Apple Silicon). Not notarized: on first
# launch of a downloaded copy, right-click → Open, or see the README.
codesign --force --deep --sign - "$APP"

DMG="$OUT/archivix-${VERSION}-macos.dmg"
mkdir -p "$WORK/dmg"
cp -R "$APP" "$WORK/dmg/"
ln -s /Applications "$WORK/dmg/Applications"
rm -f "$DMG"
hdiutil create -quiet -volname "archivix" -srcfolder "$WORK/dmg" -ov -format UDZO "$DMG"

echo "built $APP"
echo "built $DMG"
