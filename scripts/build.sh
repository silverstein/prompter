#!/bin/bash
# Build Prompter: speech recognizer + Tauri app bundle + optional install
set -e

export MACOSX_DEPLOYMENT_TARGET="13.0"

echo "=== Building speech recognizer (Swift) ==="
# Tauri bundles it as an external binary (tauri.macos.conf.json), which it
# expects under the target triple and signs along with the app.
TRIPLE="$(rustc -vV | sed -n 's/^host: //p')"
mkdir -p crates/app/binaries
# Target the app's minimum macOS (13) so the helper runs on older Macs too;
# newer features (custom language model: macOS 14) are guarded at runtime.
swiftc -O -target "$(uname -m)-apple-macos13.0" scripts/speech-recognizer.swift \
  -o "crates/app/binaries/speech-recognizer-$TRIPLE"
echo "  Built crates/app/binaries/speech-recognizer-$TRIPLE"

echo "=== Building Tauri app ==="
cd crates/app
cargo tauri build --bundles app
cd ../..

APP=target/release/bundle/macos/Prompter.app
# With a signing identity (APPLE_SIGNING_IDENTITY) Tauri signs the bundle and
# the helper. Without one, sign ad hoc so the bundle is still valid.
if ! codesign --verify --deep --strict "$APP" 2>/dev/null; then
  codesign --force --deep --options runtime \
    --entitlements crates/app/Entitlements.plist --sign - "$APP"
  echo "  Signed app bundle (ad hoc)"
fi
codesign --verify --deep --strict "$APP"

echo "=== Build complete ==="
echo "  App: target/release/bundle/macos/Prompter.app"

if [ "$1" = "--install" ]; then
    echo "=== Installing to /Applications ==="
    pkill -9 -f "prompter-app" 2>/dev/null || true
    pkill -9 -f "Prompter" 2>/dev/null || true
    sleep 2
    rm -rf /Applications/Prompter.app
    cp -R target/release/bundle/macos/Prompter.app /Applications/Prompter.app
    echo "  Installed to /Applications/Prompter.app"
    echo "  Binary: $(stat -f '%Sm' /Applications/Prompter.app/Contents/MacOS/prompter-app)"
fi
