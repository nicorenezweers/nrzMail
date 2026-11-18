#!/bin/sh
# Umbrel-Paket ohne Registry und ohne Login (wie nrzNotes):
#   scripts/umbrel-release.sh 0.3.0
# baut das Programm für linux/amd64 und linux/arm64 (Stufe `dist` des Dockerfiles, Oberfläche
# eingebettet) und legt es als Commit auf den Zweig `umbrel` mit dem Tag `umbrel-<version>`:
#   nrzmail-<arch>.gz  ca-certificates.crt  Rocket.toml  SHA256SUMS
# Das `pre-start`-Skript der App `nrz-mail` im nrzStore lädt diese Dateien auf dem Gerät von
# http://localhost:8390/nicorenezweers/nrzMail/raw/tag/umbrel-<version>/… (nrzGit) und startet
# das Programm in einem gepinnten debian:trixie-slim. Die Nachricht des Tags (Versionshinweise)
# übernimmt nrzGit als releaseNotes in den Store.
# Gepusht wird auf den Remote $REMOTE (Standard: origin = nrzGit); Docker muss laufen.
# Versionshinweise: $NOTES (Text) oder Datei $NOTES_FILE.
set -eu
cd "$(dirname "$0")/.."

version="${1:-}"
if [ -z "$version" ]; then
  echo "Aufruf: $0 <version>   (z. B. $0 0.3.0)" >&2
  exit 1
fi
remote="${REMOTE:-origin}"
tag="umbrel-$version"
out="dist/umbrel/$version"
builder="${BUILDER:-nrzmail-builder}"

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "Tag $tag gibt es schon – neue Version wählen." >&2
  exit 1
fi

rm -rf "$out"
mkdir -p "$out/bin"

echo "==> Programm (linux/amd64, linux/arm64)"
if ! docker buildx inspect "$builder" >/dev/null 2>&1; then
  docker buildx create --name "$builder" --driver docker-container >/dev/null
fi
docker buildx build --builder "$builder" --platform linux/amd64,linux/arm64 --target dist \
  --output "type=local,dest=$out/bin" .

for arch in amd64 arm64; do
  gzip -9 -c "$out/bin/linux_$arch/nrzmail" > "$out/nrzmail-$arch.gz"
done
cp "$out/bin/linux_amd64/ca-certificates.crt" "$out/ca-certificates.crt"
cp Rocket.toml "$out/Rocket.toml"
files="nrzmail-amd64.gz nrzmail-arm64.gz ca-certificates.crt Rocket.toml"
(cd "$out" && shasum -a 256 $files > SHA256SUMS)

echo "==> Zweig umbrel, Tag $tag"
worktree="$(mktemp -d -t nrzmail-umbrel.XXXXXX)"
rmdir "$worktree"
trap 'git worktree remove --force "$worktree" >/dev/null 2>&1 || true' EXIT
git fetch -q "$remote" umbrel 2>/dev/null || true
if git rev-parse -q --verify "refs/remotes/$remote/umbrel" >/dev/null; then
  git worktree add -q -B umbrel "$worktree" "$remote/umbrel"
else
  git worktree add -q --orphan -b umbrel "$worktree"
fi
rm -f "$worktree"/*.gz "$worktree"/SHA256SUMS "$worktree"/Rocket.toml "$worktree"/ca-certificates.crt
for f in $files SHA256SUMS; do cp "$out/$f" "$worktree/"; done
cat > "$worktree/README.md" <<EOT
# nrzMail – Umbrel-Paket

Dieser Zweig enthält nur fertige Dateien für die Umbrel-App \`nrz-mail\` (nrzStore), damit sie
ohne Container-Registry und ohne Login installiert werden kann. Der Quellcode liegt auf \`main\`.

| Datei | Inhalt |
|---|---|
| \`nrzmail-<arch>.gz\` | Server mit eingebetteter Oberfläche (glibc), amd64/arm64 |
| \`ca-certificates.crt\` | Zertifikate für TLS zu Mail-Servern und KI-Anbietern (Debian) |
| \`Rocket.toml\` | Standard-Konfiguration |
| \`SHA256SUMS\` | Prüfsummen |

Stand: nrzMail $version, gebaut aus \`$(git rev-parse --short HEAD)\` mit \`scripts/umbrel-release.sh $version\`.
EOT
notes="${NOTES:-}"
if [ -n "${NOTES_FILE:-}" ]; then notes="$(cat "$NOTES_FILE")"; fi
[ -n "$notes" ] || notes="nrzMail $version"
git -C "$worktree" add -A
git -C "$worktree" -c user.name="$(git config user.name || echo nrz)" -c user.email="$(git config user.email || echo nrz@localhost)" \
  commit -q -m "Umbrel-Paket nrzMail $version"
git -C "$worktree" -c user.name="$(git config user.name || echo nrz)" -c user.email="$(git config user.email || echo nrz@localhost)" \
  tag -a "$tag" -m "$notes"
git push -q "$remote" umbrel "refs/tags/$tag"
echo "Fertig: $tag auf $remote (Zweig umbrel)."
cat "$out/SHA256SUMS"
