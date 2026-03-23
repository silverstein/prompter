#!/bin/bash
# Build Prompter: CLI check + Tauri app bundle + optional install
set -e

export CXXFLAGS="-I$(xcrun --show-sdk-path)/usr/include/c++/v1"
export MACOSX_DEPLOYMENT_TARGET="13.0"

echo "=== Building Tauri app ==="
cd crates/app
cargo tauri build --bundles app
cd ../..

echo "=== Build complete ==="
echo "  App: target/release/bundle/macos/Prompter.app"

if [ "$1" = "--install" ]; then
    echo "=== Installing to /Applications ==="
    pkill -9 -f "prompter-app" 2>/dev/null || true
    pkill -9 -f "Prompter" 2>/dev/null || true
    sleep 2
    # Must rm first — cp -Rf doesn't overwrite cached macOS app bundles
    rm -rf /Applications/Prompter.app
    cp -R target/release/bundle/macos/Prompter.app /Applications/Prompter.app
    echo "  Installed to /Applications/Prompter.app"
    echo "  Binary: $(stat -f '%Sm' /Applications/Prompter.app/Contents/MacOS/prompter-app)"
fi
