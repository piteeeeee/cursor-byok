# syntax=docker/dockerfile:1.7

FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY server/ server/
COPY protocols/cursor/ protocols/cursor/
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --package cursor-server --bin cursor-server && \
    cp target/release/cursor-server /tmp/cursor-server

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install --yes --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --system --uid 10001 --home-dir /nonexistent --shell /usr/sbin/nologin cursor-byok && \
    mkdir -p /data && chown cursor-byok:cursor-byok /data

COPY --from=builder /tmp/cursor-server /usr/local/bin/cursor-server

ENV RUST_LOG=cursor_server=info

USER cursor-byok
EXPOSE 51866
VOLUME ["/data"]
HEALTHCHECK --interval=10s --timeout=3s --retries=5 \
    CMD curl --fail --silent http://127.0.0.1:51866/__byok-api__/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/cursor-server"]
