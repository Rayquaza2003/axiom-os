#!/bin/sh
# Builds dist/Axiom-<version>.dmg: Axiom.app (axiom, axion, setup) next to an Applications shortcut.
# Signing: ad-hoc by default (fine on this Mac). For other Macs set AXIOM_SIGN_ID to a "Developer ID
# Application" identity, and AXIOM_NOTARY_PROFILE to a `xcrun notarytool store-credentials` profile.
set -eu
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/shell/Cargo.toml | head -1)
app=dist/Axiom.app
cargo build --release -p axiom-shell
rm -rf dist && mkdir -p "$app/Contents/MacOS"
cp target/release/axiom target/release/axion "$app/Contents/MacOS/"
cp packaging/macos/setup.sh "$app/Contents/MacOS/setup"
chmod +x "$app/Contents/MacOS/setup"
sed "s/@VERSION@/$version/" packaging/macos/Info.plist > "$app/Contents/Info.plist"

# app icon: every size macOS asks for, from the 1024 px source
iconset=dist/Axiom.iconset && mkdir -p "$iconset" "$app/Contents/Resources"
for n in 16 32 128 256 512; do
    sips -z $n $n packaging/macos/icon.png --out "$iconset/icon_${n}x${n}.png" >/dev/null
    sips -z $((n * 2)) $((n * 2)) packaging/macos/icon.png --out "$iconset/icon_${n}x${n}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Axiom.icns" && rm -rf "$iconset"

if [ -n "${AXIOM_SIGN_ID:-}" ]; then
    for f in axiom axion setup; do
        codesign --force --options runtime --timestamp -s "$AXIOM_SIGN_ID" "$app/Contents/MacOS/$f"
    done
    codesign --force --options runtime --timestamp -s "$AXIOM_SIGN_ID" "$app"
else
    codesign --force --deep -s - "$app"
fi

stage=dist/stage && mkdir -p "$stage" && cp -R "$app" "$stage/" && ln -s /Applications "$stage/Applications"
dmg="dist/Axiom-$version.dmg"
hdiutil create -volname "Axiom $version" -srcfolder "$stage" -ov -format UDZO "$dmg" >/dev/null
rm -rf "$stage"
if [ -n "${AXIOM_SIGN_ID:-}" ]; then
    codesign -s "$AXIOM_SIGN_ID" --timestamp "$dmg"
    if [ -n "${AXIOM_NOTARY_PROFILE:-}" ]; then
        xcrun notarytool submit "$dmg" --keychain-profile "$AXIOM_NOTARY_PROFILE" --wait
        xcrun stapler staple "$dmg"
    fi
fi
echo "$dmg"
