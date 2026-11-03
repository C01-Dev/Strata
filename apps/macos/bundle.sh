#!/usr/bin/env bash
# Builds build/Strata.app: the menu bar app, signed ad hoc for this machine.
# Usage: apps/macos/bundle.sh   then   open apps/macos/build/Strata.app
set -euo pipefail
cd "$(dirname "$0")"
swift build -c release
app=build/Strata.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp "$(swift build -c release --show-bin-path)/StrataBar" "$app/Contents/MacOS/Strata"
cp Info.plist "$app/Contents/"
codesign --force --sign - "$app"
echo "$app"
