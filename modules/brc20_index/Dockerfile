# Stage 1: Build the Rust application
FROM rust:1.88.0-bookworm AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    clang \
    libclang-dev \
    && rm -rf /var/lib/apt/lists/*

# Set the working directory
WORKDIR /usr/src/app
COPY db_reader /usr/ord/db_reader
# Copy the Cargo.toml and Cargo.lock files
COPY Cargo.toml Cargo.lock ./

# Copy the source code
COPY src ./src

# Build the application in release mode
# The DATABASE_URL must be provided as a build argument
ARG DATABASE_URL
RUN cargo build --release

# Stage 2: Create a small final image
FROM gcr.io/distroless/cc-debian12

# Set the working directory
WORKDIR /app

# Copy the compiled binary from the builder stage to a location in PATH
COPY --from=builder /usr/src/app/target/release/brc20-index /usr/local/bin/brc20-index

# Expose any necessary ports (if your application listens on a port)
# EXAMPLE: EXPOSE 8080

# Command to run the application
ENTRYPOINT ["brc20-index"]
