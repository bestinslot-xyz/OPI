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

# Create a dummy Cargo.toml with the dependencies we want to pre-compile
RUN echo '[package]\n\
name = "dummy"\n\
version = "0.1.0"\n\
edition = "2021"\n\
\n\
[dependencies]\n\
rocksdb = "0.23.0"\n\
rlimit = "0.10.2"\n\
jsonrpsee = { version = "0.25.1", features = [\n\
    "server",\n\
    "client",\n\
    "macros",\n\
    "jsonrpsee-core",\n\
    "jsonrpsee-types",\n\
] }\n\
tower = "0.5.2"\n\
tower-http = { version = "0.6.2", features = ["auth", "compression-br", "compression-gzip", "cors", "set-header"] }\n\
tokio = { version = "1.43.0", features = ["rt-multi-thread", "signal"] }\n\
hyper = { version = "1.5.2", features = ["client", "http2"] }\n\
hyper-util = { version = "0.1.10", features = ["client", "client-legacy", "http2", "tokio"] }\n\
serde = { version = "1.0.137", features = ["derive"] }\n\
hex = "0.4.3"\n\
serde-hex = "0.1.0"\n\
serde_json = { version = "1.0.81", features = ["preserve_order"] }\n\
bitcoin = { version = "0.32.5", features = ["rand"] }\n\
ctrlc = "3.4.7"\n\
signal-hook = "0.3.18"' > Cargo.toml

# Create a dummy main.rs
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs

# Pre-compile all dependencies in release mode
RUN cargo build --release

# Clean up everything except the compiled dependencies
RUN rm -rf src Cargo.toml Cargo.lock target/release/deps/dummy* target/release/dummy* target/release/build/dummy*

# Final stage - much smaller base image
FROM rust:1.86.0-bookworm

# Install only runtime dependencies
RUN apt-get update && apt-get install -y \
    libclang-dev \
    clang \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/* \
    && apt-get clean

# Copy pre-compiled dependencies from the previous stage
COPY --from=deps-builder /tmp/deps/target /usr/local/cargo/registry-cache

# Create a script to setup the cargo cache for new projects
RUN echo '#!/bin/bash\n\
if [ ! -d "/usr/src/app/target" ]; then\n\
    mkdir -p /usr/src/app/target\n\
    cp -r /usr/local/cargo/registry-cache/* /usr/src/app/target/ 2>/dev/null || true\n\
fi' > /usr/local/bin/setup-cargo-cache && chmod +x /usr/local/bin/setup-cargo-cache

WORKDIR /usr/src/app

# Set environment variable to help with cargo cache
ENV CARGO_TARGET_DIR=/usr/src/app/target
