# nrzMail – Umbrel-Paket

Dieser Zweig enthält nur fertige Dateien für die Umbrel-App `nrz-mail` (nrzStore), damit sie
ohne Container-Registry und ohne Login installiert werden kann. Der Quellcode liegt auf `main`.

| Datei | Inhalt |
|---|---|
| `nrzmail-<arch>.gz` | Server mit eingebetteter Oberfläche (glibc), amd64/arm64 |
| `ca-certificates.crt` | Zertifikate für TLS zu Mail-Servern und KI-Anbietern (Debian) |
| `Rocket.toml` | Standard-Konfiguration |
| `SHA256SUMS` | Prüfsummen |

Stand: nrzMail 0.4.0, gebaut aus `cf5365c` mit `scripts/umbrel-release.sh 0.4.0`.
