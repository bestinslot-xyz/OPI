#####################################################################
# Stage 0 — build RocksDB once (static, portable)                   #
#####################################################################
FROM debian:bookworm-slim AS rocks-build

ARG ROCKS_VERSION=9.3.1

# tool-chain + codecs + bash ────────────────────────────────────────
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        bash git ca-certificates build-essential \
        libsnappy-dev libzstd-dev libbz2-dev liblz4-dev zlib1g-dev && \
    rm -rf /var/lib/apt/lists/*

# clone & compile ───────────────────────────────────────────────────
RUN git clone --branch v${ROCKS_VERSION} --depth 1 \
        https://github.com/facebook/rocksdb.git /rocksdb && \
    cd /rocksdb && \
    PORTABLE=1 make -j"$(nproc)" static_lib

# collect artefacts ⤵︎                                              
RUN mkdir /out && \
    cp librocksdb.a include/ /out && \
    # copy the static compressor libs RocksDB linked against
    cp /usr/lib/x86_64-linux-gnu/lib{snappy,lz4,zstd,bz2,z}.a /out && \
    strip --strip-unneeded /out/librocksdb.a

#####################################################################
# Stage 1 — the lean, reusable rocksdb base image (~30 MB)          #
#####################################################################
FROM debian:bookworm-slim AS rocksdb-base

COPY --from=rocks-build /out /usr/local/rocksdb

ENV ROCKSDB_LIB_DIR=/usr/local/rocksdb \
    ROCKSDB_INCLUDE_DIR=/usr/local/rocksdb/include \
    ROCKSDB_STATIC=1 \
    LIBRARY_PATH=/usr/local/rocksdb${LIBRARY_PATH:+:$LIBRARY_PATH}

# nothing else – ship it! 🎉
