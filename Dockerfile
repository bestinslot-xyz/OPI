# syntax=docker/dockerfile:1

# ord 0.29.1 (linked as a library) sets rust-version 1.95.
FROM rust:1.95-bookworm AS builder

# librocksdb-sys (brc20-prog) compiles C++ and runs bindgen.
RUN apt-get update \
    && apt-get install -y --no-install-recommends clang libclang-dev cmake pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY . .

ENV SQLX_OFFLINE=true
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked -p opi-indexer \
    && install -D target/release/opi-indexer /out/opi-indexer

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libstdc++6 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --user-group --no-create-home --shell /usr/sbin/nologin opi \
    && mkdir -p /data/journal /data/ord /data/brc20-prog \
    && chown -R 10001:10001 /data

COPY --from=builder /out/opi-indexer /usr/local/bin/opi-indexer

# All state lives under /data: mount a volume there.
ENV OPI_JOURNAL_DIR=/data/journal \
    OPI_ORD_DATA_DIR=/data/ord \
    BRC20_PROG_DB_PATH=/data/brc20-prog \
    API_HOST=0.0.0.0 \
    API_PORT=8000

WORKDIR /data
USER 10001:10001
VOLUME ["/data"]
EXPOSE 8000

ENTRYPOINT ["opi-indexer"]
CMD ["run"]
