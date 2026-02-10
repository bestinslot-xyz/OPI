FROM rust:1.86.0-bookworm as deps-builder

# Install libclang and other dependencies needed for rocksdb
RUN apt-get update && apt-get install -y \
    libclang-dev \
    clang \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Set up a workspace for pre-compiling dependencies
WORKDIR /tmp/deps

# Create a dummy Cargo.toml with ONLY the heavy dependencies we want to cache
RUN echo '[package]\n\
name = "dummy"\n\
version = "0.1.0"\n\
edition = "2021"\n\
\n\
[dependencies]\n\
rocksdb = "0.23.0"' > Cargo.toml

# Create a dummy main.rs that uses the dependency
RUN mkdir -p src && echo 'use rocksdb::DB; fn main() { println!("deps compiled"); }' > src/main.rs

# Pre-compile ONLY rocksdb (the heaviest dependency)
RUN cargo build --release

# Extract only the compiled rocksdb artifacts
RUN mkdir -p /compiled-deps && \
    find target/release/deps -name "*rocksdb*" -type f -exec cp {} /compiled-deps/ \; && \
    find target/release/build -name "*rocksdb*" -type d -exec cp -r {} /compiled-deps/ \; 2>/dev/null || true

# Final minimal stage
FROM rust:1.86.0-bookworm

# Install only essential build dependencies
RUN apt-get update && apt-get install -y \
    libclang-dev \
    clang \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/* \
    && apt-get clean \
    && rm -rf /usr/share/doc/* \
    && rm -rf /usr/share/man/* \
    && rm -rf /usr/share/locale/*

# Create cargo registry cache directory structure
RUN mkdir -p /usr/local/cargo/registry/cache \
    /usr/local/cargo/registry/src \
    /usr/local/cargo/git/db

# Copy only the essential compiled rocksdb artifacts
COPY --from=deps-builder /compiled-deps/* /usr/local/cargo/registry/cache/ 2>/dev/null || true

# Create a helper script to initialize target directory for builds
RUN echo '#!/bin/bash\n\
echo "Setting up cargo cache for faster RocksDB builds..."\n\
# The actual speedup comes from having the dependencies in cargo registry\n\
# Individual projects will still need to compile, but dependency resolution will be faster' > /usr/local/bin/init-cargo-cache \
    && chmod +x /usr/local/bin/init-cargo-cache

WORKDIR /usr/src/app

# Pre-download and cache the rocksdb crate source
RUN cargo search rocksdb --limit 1 >/dev/null 2>&1 || true

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
