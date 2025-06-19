FROM rust:1.86.0-bookworm as builder

# Install libclang and other dependencies needed for rocksdb
RUN apt-get update && apt-get install -y \
    libclang-dev \
    clang \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/db_reader

COPY . .

RUN cargo build --bin db_reader --release

FROM debian:bookworm-slim

COPY --from=builder /usr/src/db_reader/target/release/db_reader /usr/local/bin
RUN apt-get update && apt-get install -y openssl

CMD ["sh", "-c", "db_reader"]
