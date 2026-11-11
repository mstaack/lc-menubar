#!/usr/bin/env bash
# Builds dist/LC Menubar.app (universal: Apple silicon + Intel) and a zip with a
# SHA-256 file, ready for a GitHub release and the in-app updater.
#
# Optional signing and notarization (all three steps are skipped when unset):
#   SIGN_IDENTITY   "Developer ID Application: Name (TEAMID)"; ad-hoc signed otherwise
#   NOTARY_PROFILE  a `xcrun notarytool store-credentials` profile name, or
#   APPLE_ID + APPLE_TEAM_ID + APPLE_APP_PASSWORD
# UNIVERSAL=0 builds for this Mac's architecture only (faster, for local tests).
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DIST=dist
APP="$DIST/LC Menubar.app"
ZIP="$DIST/lc-menubar-$VERSION-macos-universal.zip"
export MACOSX_DEPLOYMENT_TARGET=13.0

rm -rf "$DIST" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

if [[ "${UNIVERSAL:-1}" == 1 ]]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  cargo build --release --locked --target aarch64-apple-darwin
  cargo build --release --locked --target x86_64-apple-darwin
  lipo -create -output "$APP/Contents/MacOS/lc-menubar" \
    target/aarch64-apple-darwin/release/lc-menubar target/x86_64-apple-darwin/release/lc-menubar
else
  cargo build --release --locked
  cp target/release/lc-menubar "$APP/Contents/MacOS/lc-menubar"
fi

sed "s/__VERSION__/$VERSION/g" packaging/Info.plist > "$APP/Contents/Info.plist"
ICONSET="$DIST/AppIcon.iconset"
"$APP/Contents/MacOS/lc-menubar" --app-icon "$ICONSET"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
rm -rf "$ICONSET"

if [[ -n "${SIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
else
  codesign --force --sign - "$APP"
fi
codesign --verify --strict "$APP"

ditto -c -k --keepParent "$APP" "$ZIP"

if [[ -n "${SIGN_IDENTITY:-}" ]] && [[ -n "${NOTARY_PROFILE:-}${APPLE_ID:-}" ]]; then
  if [[ -n "${NOTARY_PROFILE:-}" ]]; then
    xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
  else
    xcrun notarytool submit "$ZIP" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
      --password "$APPLE_APP_PASSWORD" --wait
  fi
  xcrun stapler staple "$APP"
  rm "$ZIP" && ditto -c -k --keepParent "$APP" "$ZIP"
fi

(cd "$DIST" && shasum -a 256 "$(basename "$ZIP")" > "$(basename "$ZIP").sha256")
echo "Built $ZIP"
cat "$ZIP.sha256"
