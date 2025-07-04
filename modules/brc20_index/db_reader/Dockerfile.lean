FROM registry.bestinslot.xyz/rust-rocksdb-base:lean as builder

WORKDIR /usr/src/db_reader

# Copy dependency files first for better layer caching
COPY Cargo.toml Cargo.lock ./

# Pre-compile dependencies (this will be much faster with the lean base)
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src

# Copy actual source code
COPY src ./src

# Build the actual application
RUN cargo build --bin db_reader --release

FROM debian:bookworm-slim

COPY --from=builder /usr/src/db_reader/target/release/db_reader /usr/local/bin
RUN apt-get update && apt-get install -y openssl ca-certificates && rm -rf /var/lib/apt/lists/*

CMD ["sh", "-c", "db_reader"]
