# syntax=docker/dockerfile:1.10
# nrzMail – Multi-Stage-Build für amd64 und arm64: Oberfläche (Dioxus/WebAssembly) und Programm
# (Rust). Braucht nur Docker – kein Rust auf dem Rechner.
#   docker build -t nrzmail .
#   docker run -p 8330:8330 -v nrzmail-data:/data nrzmail
# Beide Architekturen (Rust wird quer-übersetzt, nicht emuliert):
#   docker buildx build --platform linux/amd64,linux/arm64 -t <registry>/nrz-mail:<version> --push .

ARG DEBIAN=debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8

ARG RUST=rust:1.98-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e

# ---------------------------------------------------------------- Oberfläche
# WebAssembly ist überall gleich – einmal auf dem Build-Rechner bauen, für alle Architekturen.
FROM --platform=$BUILDPLATFORM ${RUST} AS ui
RUN rustup target add wasm32-unknown-unknown
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    cargo install dioxus-cli --version 0.7.10 --locked --root /opt/dx
WORKDIR /src
# Die Oberfläche benutzt dieselben API-Typen wie der Server (shared/)
COPY shared shared
COPY ui ui
# `dx bundle` kopiert alles aus seinem Zwischenordner mit – den darum vorher leeren
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/ui/target,id=nrzmail-ui-target \
    cd ui && rm -rf target/dx/nrzmail-ui/release/web/public \
 && PATH=/opt/dx/bin:$PATH dx bundle --web --release --out-dir /tmp/ui \
 && mkdir -p /out && cp -r /tmp/ui/public /out/public && test -f /out/public/index.html

# ---------------------------------------------------------------- Programm
FROM --platform=$BUILDPLATFORM ${RUST} AS build
ARG TARGETARCH
ARG BUILDARCH
# Querübersetzer, wenn Ziel und Build-Rechner verschieden sind (SQLCipher, OpenSSL und ring
# werden mit dem C-Compiler des Ziels gebaut).
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) t=x86_64-unknown-linux-gnu; p=x86_64-linux-gnu; pkg="gcc-x86-64-linux-gnu libc6-dev-amd64-cross" ;; \
      arm64) t=aarch64-unknown-linux-gnu; p=aarch64-linux-gnu; pkg="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
      *) echo "Architektur $TARGETARCH nicht unterstützt" >&2; exit 1 ;; \
    esac; \
    echo "$t" > /rust-target; \
    if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update && apt-get install -y --no-install-recommends $pkg && rm -rf /var/lib/apt/lists/*; \
      rustup target add "$t"; \
      printf '[target.%s]\nlinker = "%s-gcc"\n' "$t" "$p" >> /usr/local/cargo/config.toml; \
      u=$(echo "$t" | tr - _); \
      printf 'CC_%s=%s-gcc\nAR_%s=%s-ar\n' "$u" "$p" "$u" "$p" > /cross-env; \
    else \
      touch /cross-env; \
    fi
WORKDIR /src
# Der ganze Quelltext (was nicht hineingehört, steht in .dockerignore) – so fehlt nichts, was
# build.rs oder include_bytes! braucht (Migrationen, static/, locales/ …).
COPY . .
# Oberfläche aus der Stufe oben; build.rs bettet sie ins Programm ein (Text und WebAssembly
# gzip) – der Server braucht dafür keine Dateien mehr.
COPY --from=ui /out/public ui/dist/public
# Optional ein eingebauter OpenRouter-Schlüssel als Build-Secret – nie im Quelltext oder in den
# Image-Schichten. Ohne Secret: KI-Anbieter und Schlüssel in den Einstellungen bzw. per Umgebung.
#   --secret id=openrouter,env=NRZ_OPENROUTER_KEY   (Releases dann mit --no-cache-filter build:
#   BuildKit nimmt Secrets nicht in den Cache-Schlüssel)
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/src/target,id=nrzmail-target-$TARGETARCH \
    --mount=type=secret,id=openrouter,env=NRZ_OPENROUTER_KEY \
    echo "Eingebauter KI-Schlüssel: $([ -n "${NRZ_OPENROUTER_KEY:-}" ] && echo ja || echo nein)" \
 && env $(cat /cross-env) cargo build --release --locked --target "$(cat /rust-target)" \
 && cp "target/$(cat /rust-target)/release/nrzmail" /nrzmail \
 && if [ -n "${NRZ_OPENROUTER_KEY:-}" ] && ! grep -qF "$NRZ_OPENROUTER_KEY" /nrzmail; then \
      echo "Der KI-Schlüssel fehlt im Programm (alter Stand aus dem Cache?)" >&2; exit 1; \
    fi \
 && mkdir /data-empty

# ---------------------------------------------------------------- Laufzeit
# (`dist` am Ende: nur Programm und Zertifikate für das Umbrel-Paket, scripts/umbrel-release.sh)
# Schlankes Debian; ca-certificates für TLS zu Mail-Servern und KI-Anbietern.
FROM ${DEBIAN} AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 1000 --home-dir /data --shell /usr/sbin/nologin nrzmail
COPY --from=build --chown=1000:1000 /data-empty /data
COPY --from=build /nrzmail /usr/local/bin/nrzmail
COPY Rocket.toml /app/Rocket.toml

WORKDIR /app
ENV NRZMAIL_CONFIG=/app/Rocket.toml \
    NRZMAIL_DATA_DIR=/data \
    NRZMAIL_ADDRESS=0.0.0.0 \
    NRZMAIL_PORT=8330 \
    NRZMAIL_LOG=info,rocket::server=warn,rocket::launch::_=warn,rocket::shield=warn
USER nrzmail
VOLUME /data
EXPOSE 8330
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD ["nrzmail", "healthcheck"]
STOPSIGNAL SIGTERM
CMD ["nrzmail"]

# ---------------------------------------------------------------- Umbrel-Paket
# Nur die Dateien für scripts/umbrel-release.sh (je Architektur):
#   docker buildx build --platform linux/amd64,linux/arm64 --target dist --output type=local,dest=out .
FROM scratch AS dist
COPY --from=build /nrzmail /nrzmail
COPY --from=runtime /etc/ssl/certs/ca-certificates.crt /ca-certificates.crt
