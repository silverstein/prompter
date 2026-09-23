#!/bin/bash
# Build Prompter: speech recognizer + Tauri app bundle + optional install
set -e

export CXXFLAGS="-I$(xcrun --show-sdk-path)/usr/include/c++/v1"
export MACOSX_DEPLOYMENT_TARGET="13.0"

echo "=== Building speech recognizer (Swift) ==="
mkdir -p target
# Target the app's minimum macOS (13) so the helper runs on older Macs too;
# newer features (custom language model: macOS 14) are guarded at runtime.
swiftc -O -target "$(uname -m)-apple-macos13.0" scripts/speech-recognizer.swift -o target/speech-recognizer
echo "  Built target/speech-recognizer"

echo "=== Building Tauri app ==="
cd crates/app
cargo tauri build --bundles app
cd ../..

echo "=== Embedding speech recognizer in app bundle ==="
cp target/speech-recognizer target/release/bundle/macos/Prompter.app/Contents/MacOS/speech-recognizer
echo "  Embedded speech-recognizer in app bundle"
# Adding a file after Tauri signed the bundle invalidates its signature (and
# Gatekeeper then calls a downloaded copy "damaged"): re-sign ad hoc.
codesign --force --deep --sign - target/release/bundle/macos/Prompter.app
codesign --verify --deep --strict target/release/bundle/macos/Prompter.app
echo "  Re-signed app bundle (ad hoc)"

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
