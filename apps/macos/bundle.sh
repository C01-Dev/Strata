#!/usr/bin/env bash
# Builds build/Strata.app, the menu bar app.
#
#   apps/macos/bundle.sh             # signed ad hoc, for this machine
#   apps/macos/bundle.sh --release   # Developer ID, notarized and stapled: build/Strata-macOS.zip
#
# --release needs a "Developer ID Application" certificate in the keychain and a notarytool
# profile saved with `xcrun notarytool store-credentials strata-notary …` (NOTARY_PROFILE to
# use another name). The password stays in the keychain; this script never sees it.
set -euo pipefail
cd "$(dirname "$0")"
swift build -c release
app=build/Strata.app
rm -rf "$app" build/Strata-macOS.zip
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$(swift build -c release --show-bin-path)/StrataBar" "$app/Contents/MacOS/Strata"
cp Info.plist "$app/Contents/"
# icon.png is rendered by C01's Remotion project (src/strata/Icon.tsx); AppIcon.icns is built from it.
cp AppIcon.icns "$app/Contents/Resources/"

if [ "${1:-}" != "--release" ]; then
  codesign --force --sign - "$app"
  echo "$app"
  exit 0
fi

identity=$(security find-identity -v -p codesigning | sed -n 's/.*"\(Developer ID Application: .*\)"/\1/p' | head -1)
[ -n "$identity" ] || { echo "no Developer ID Application certificate in the keychain" >&2; exit 1; }
codesign --force --options runtime --timestamp --sign "$identity" "$app"
codesign --verify --strict --verbose=2 "$app"
ditto -c -k --keepParent "$app" build/notarize.zip
xcrun notarytool submit build/notarize.zip --keychain-profile "${NOTARY_PROFILE:-strata-notary}" --wait
rm build/notarize.zip
xcrun stapler staple "$app"
spctl --assess --type execute --verbose "$app"
ditto -c -k --keepParent "$app" build/Strata-macOS.zip
shasum -a 256 build/Strata-macOS.zip
