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
    pkill -f "Prompter" 2>/dev/null || true
    sleep 1
    cp -Rf target/release/bundle/macos/Prompter.app /Applications/Prompter.app
    echo "  Installed to /Applications/Prompter.app"
fi
