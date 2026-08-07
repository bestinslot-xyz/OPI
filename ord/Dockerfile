FROM rust:1.86.0-bookworm AS builder

# Install additional build dependencies
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        libssl-dev \
        libclang-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /usr/src/ord

COPY . .

RUN cargo build --bin ord --release

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /usr/src/ord/target/release/ord /usr/local/bin/ord

ENV RUST_BACKTRACE=1
ENV RUST_LOG=info

ENTRYPOINT ["ord"]

