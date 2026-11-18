#!/bin/sh
# macOS-App bauen: dist-macos/nrzMail.app und dist-macos/nrzMail-<version>-<arch>.dmg
#   scripts/macos.sh            (Apple Silicon bzw. die Architektur dieses Macs)
#   UNIVERSAL=1 scripts/macos.sh  (arm64 + x86_64 in einem Programm)
# Baut vorher die Oberfläche (scripts/ui.sh), ausser SKIP_UI=1 und sie liegt schon vor.
# Signatur: SIGN_IDENTITY="Developer ID Application: …" (Apple-Konto), sonst ad hoc.
set -eu
cd "$(dirname "$0")/.."
root="$(pwd)"

if [ -d "$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin" ]; then
  PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
fi
PATH="$HOME/.cargo/bin:$PATH"
export PATH

ver() { sed -n 's/^version = "\(.*\)"/\1/p' "$1" | head -1; }
version="$(ver Cargo.toml)"
app_version="$(ver macos/Cargo.toml)"
if [ "$version" != "$app_version" ]; then
  echo "Version in Cargo.toml ($version) und macos/Cargo.toml ($app_version) ist verschieden" >&2
  exit 1
fi

if [ "${SKIP_UI:-}" != "1" ] || [ ! -f ui/dist/public/index.html ]; then
  sh scripts/ui.sh
fi

cd macos
if [ "${UNIVERSAL:-}" = "1" ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin
  cargo build --release --target aarch64-apple-darwin
  cargo build --release --target x86_64-apple-darwin
  mkdir -p target/universal
  lipo -create -output target/universal/nrzmail-macos \
    target/aarch64-apple-darwin/release/nrzmail-macos target/x86_64-apple-darwin/release/nrzmail-macos
  bin="$root/macos/target/universal/nrzmail-macos"; arch=universal
else
  cargo build --release
  bin="$root/macos/target/release/nrzmail-macos"
  case "$(uname -m)" in arm64) arch=arm64 ;; *) arch=x86_64 ;; esac
fi
cd "$root"

out="$root/dist-macos"
app="$out/nrzMail.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/nrzmail-macos"
sed "s/__VERSION__/$version/g" macos/resources/Info.plist > "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# Symbol aus static/icon.png (alle Grössen, mit @2x)
tmp="$(mktemp -d)"
set_dir="$tmp/nrzMail.iconset"
mkdir -p "$set_dir"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" static/icon.png --out "$set_dir/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z "$d" "$d" static/icon.png --out "$set_dir/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$set_dir" -o "$app/Contents/Resources/nrzMail.icns"

if [ -n "${SIGN_IDENTITY:-}" ] && [ "${SIGN_IDENTITY}" != "-" ]; then
  codesign --force --deep --options runtime --timestamp --sign "$SIGN_IDENTITY" "$app"
else
  codesign --force --deep --sign - "$app"
fi
codesign --verify --deep --strict "$app"

# DMG: App plus Verknüpfung auf /Programme zum Hineinziehen
dmg="$out/nrzMail-$version-$arch.dmg"
stage="$tmp/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -volname "nrzMail $version" -srcfolder "$stage" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$dmg" >/dev/null
rm -rf "$tmp"
(cd "$out" && shasum -a 256 "$(basename "$dmg")" > SHA256SUMS)
echo "Fertig: $app"
echo "        $dmg ($(du -h "$dmg" | cut -f1))"
