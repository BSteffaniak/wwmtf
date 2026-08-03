# syntax=docker/dockerfile:1.7

FROM rust:1.95-bookworm AS builder

WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml rustfmt.toml ./
COPY .cargo .cargo
COPY packages packages

RUN cargo build --locked --release -p wwmtf_app --bin wwmtf

FROM debian:bookworm-slim AS runtime

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    nginx \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /etc/nginx/sites-enabled/default \
    && groupadd --system --gid 10001 app \
    && useradd --system --uid 10001 --gid app --home-dir /app app \
    && mkdir -p /app /data /var/cache/nginx /var/lib/nginx /var/log/nginx \
    && chown -R app:app /app /data /var/cache/nginx /var/lib/nginx /var/log/nginx /run

WORKDIR /app
COPY --from=builder /app/target/release/wwmtf ./wwmtf
COPY config/nginx.conf /etc/nginx/nginx.conf
COPY scripts/container-entrypoint.sh ./container-entrypoint.sh

USER app:app

EXPOSE 8080

ENV WWMTF_PRODUCTION_MODE=true \
    WWMTF_BIND_ADDRESS=127.0.0.1 \
    WWMTF_PORT=8343 \
    WWMTF_DATABASE_PATH=/data/wwmtf.db \
    WWMTF_PUBLIC_BASE_URL=https://wwmtf.hyperchad.dev \
    RUST_LOG=info,moosicbox_middleware::api_logger=off

CMD ["./container-entrypoint.sh"]
