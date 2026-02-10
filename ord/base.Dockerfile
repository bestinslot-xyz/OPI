######################################      org.opencontainers.image.version="7.10.2" \
      org.opencontainers.image.description="Rust toolchain with RocksDB 7.10.2"#############################
# Stage 0 – build RocksDB 7.10.2 once (static + portable)            #
#####################################################################
FROM debian:bookworm-slim AS rocks-build
ARG ROCKS_VERSION=7.10.2

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        bash git ca-certificates build-essential \
        libsnappy-dev liblz4-dev libzstd-dev libbz2-dev zlib1g-dev && \
    rm -rf /var/lib/apt/lists/*

RUN git clone --branch v${ROCKS_VERSION} --depth 1 \
        https://github.com/facebook/rocksdb.git /rocksdb && \
    cd /rocksdb && \
    PORTABLE=1 make -j"$(nproc)" static_lib

# ─── gather artefacts ──────────────────────────────────────────────
WORKDIR /rocksdb
RUN set -e; \
    ARCHDIR=$(dpkg-architecture -qDEB_HOST_MULTIARCH); \
    echo "Detected multi-arch lib dir: /usr/lib/${ARCHDIR}"; \
    mkdir /out; \
    # rocksdb itself
    cp librocksdb.a /out/ && \
    cp -r include /out/ && \
    # compressor static libs
    for L in snappy lz4 zstd bz2 z; do \
        echo "Copying /usr/lib/${ARCHDIR}/lib${L}.a"; \
        cp /usr/lib/${ARCHDIR}/lib${L}.a /out/ ; \
    done && \
    strip --strip-unneeded /out/librocksdb.a

#####################################################################
# Stage 1 – comprehensive Rust + RocksDB base image                 #
#####################################################################
FROM debian:bookworm-slim AS rocksdb-base
LABEL org.opencontainers.image.title="rust-rocksdb-base" \
      org.opencontainers.image.version="8.10.2" \
      org.opencontainers.image.description="Rust toolchain with RocksDB 8.10.2"

# Install build dependencies and tools
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        curl ca-certificates build-essential \
        libssl-dev pkg-config libclang-dev \
        git && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

# Install Rust toolchain
ARG RUST_VERSION=1.86.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
    sh -s -- -y --default-toolchain ${RUST_VERSION} && \
    /root/.cargo/bin/rustup component add clippy rustfmt

# Copy RocksDB artifacts
COPY --from=rocks-build /out /usr/local/rocksdb

# Set environment variables
ENV ROCKSDB_LIB_DIR=/usr/local/rocksdb \
    ROCKSDB_INCLUDE_DIR=/usr/local/rocksdb/include \
    ROCKSDB_STATIC=1 \
    LIBRARY_PATH=/usr/local/rocksdb${LIBRARY_PATH:+:$LIBRARY_PATH} \
    PATH="/root/.cargo/bin:${PATH}" \
