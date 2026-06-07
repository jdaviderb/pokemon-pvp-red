# syntax=docker/dockerfile:1
# Production image for nes-web. Targets linux/amd64 — build with:
#   docker buildx build --platform linux/amd64 -t nes-web:prod --load .
# (or ./build-docker-production.sh). Single-container = --solo (one process, one port). The scalable
# coordinator+worker model needs orchestration — see docs/SCALING.md.

# ---------- builder ----------
FROM rust:1.92-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
        clang pkg-config libvpx-dev libopus-dev curl unzip ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# The repo's .cargo/config.toml points PKG_CONFIG_PATH/LIBRARY_PATH at macOS homebrew. cargo's [env]
# does NOT override an env var already set in the process, so set the Linux paths here to win.
ENV PKG_CONFIG_PATH=/usr/lib/x86_64-linux-gnu/pkgconfig:/usr/share/pkgconfig \
    LIBRARY_PATH=/usr/lib/x86_64-linux-gnu
WORKDIR /src
COPY . .
RUN cargo build --release && cp target/release/nes-web /nes-web
# Linux libretro core (the repo's cores/ hold macOS .dylib only).
RUN curl -fsSL -o /tmp/gb.zip \
      https://buildbot.libretro.com/nightly/linux/x86_64/latest/gambatte_libretro.so.zip \
    && (cd /tmp && unzip -o gb.zip) && cp /tmp/gambatte_libretro.so /gambatte_libretro.so

# ---------- runtime ----------
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
        libvpx7 libopus0 libstdc++6 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /nes-web /app/nes-web
COPY --from=builder /gambatte_libretro.so /app/cores/gambatte_libretro.so
COPY static /app/static
COPY states /app/states
COPY ["Pokemon Red.gb", "/app/Pokemon Red.gb"]
# 0.0.0.0 so Docker can publish the port; DEV unset => prod (no /battle/* or /console).
ENV BIND_ADDR=0.0.0.0 \
    RUST_LOG=nes_web=info,webrtc=warn
EXPOSE 3000
CMD ["/app/nes-web", "--solo", "Pokemon Red.gb", "cores/gambatte_libretro.so"]
