# syntax=docker/dockerfile:1.7
#
# stenly-share: one small image for both the hosted default and self-hosting.
#
# hearth (the web framework) is a public GitHub repo fetched anonymously over
# HTTPS at a pinned revision; no credentials are needed:
#
#   cd server/stenly-share
#   docker build -t stenly-share .

ARG RUST_VERSION=1.99.1

# ---------------------------------------------------------------- build
FROM rust:${RUST_VERSION}-slim-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates pkg-config \
 && rm -rf /var/lib/apt/lists/*
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true CARGO_TERM_COLOR=never
WORKDIR /src

# The asset bundler (`topcoat asset bundle`), pinned to the framework's version.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install --locked --git https://github.com/wbunting/hearth.git --rev 82875db6f13aa5f3f333bdfdbf68e97d3b04dff3 topcoat-cli

COPY rust-toolchain.toml Cargo.toml Cargo.lock build.rs ./
COPY .cargo .cargo
COPY src src
COPY web web
COPY assets assets
COPY migrations migrations

# Build the binary, then scan it and bundle its assets (CSS, JS, fonts) next
# to it. Caches keep rebuilds fast; the outputs are copied out of the cache.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin stenly-share \
 && topcoat asset bundle --release \
 && mkdir -p /out \
 && cp target/release/stenly-share /out/ \
 && cp -r target/release/assets /out/assets

# ---------------------------------------------------------------- runtime
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin stenly \
 && mkdir -p /data && chown stenly /data
COPY --from=build /out/stenly-share /app/stenly-share
COPY --from=build /out/assets /app/assets

ENV HOST=0.0.0.0 \
    PORT=8080 \
    STENLY_SHARE_DATA_DIR=/data \
    STENLY_SHARE_ASSETS_DIR=/app/assets
USER stenly
VOLUME ["/data"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
  CMD curl -fsS "http://127.0.0.1:${PORT}/healthz" || exit 1
ENTRYPOINT ["/app/stenly-share"]
