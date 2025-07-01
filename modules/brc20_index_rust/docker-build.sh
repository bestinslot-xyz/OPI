#!/bin/bash
set -e

echo "Creating Docker network for build..."
docker network create brc20-build-network 2>/dev/null || true

echo "Starting PostgreSQL container for build..."

# Start PostgreSQL container
docker run -d \
    --name postgres-brc20-build \
    --network brc20-build-network \
    --network-alias postgres-build \
    -e POSTGRES_USER=postgres \
    -e POSTGRES_PASSWORD=postgres \
    -e POSTGRES_DB=brc20_index \
    postgres:16-alpine

# Wait for PostgreSQL to be ready
echo "Waiting for PostgreSQL to be ready..."
for i in {1..30}; do
    if docker exec postgres-brc20-build pg_isready -U postgres > /dev/null 2>&1; then
        echo "PostgreSQL is ready!"
        break
    fi
    if [ $i -eq 30 ]; then
        echo "PostgreSQL failed to start in time"
        docker logs postgres-brc20-build
        docker rm -f postgres-brc20-build
        docker network rm brc20-build-network
        exit 1
    fi
    sleep 1
done

# Initialize database schema
echo "Initializing database schema..."
docker exec -i postgres-brc20-build psql -U postgres -d brc20_index < src/database/sql/db_init.sql

# Build the Docker image with DATABASE_URL using network mode
echo "Building Docker image..."
docker build \
    --network brc20-build-network \
    --build-arg DATABASE_URL="postgres://postgres:postgres@postgres-build:5432/brc20_index" \
    -t brc20-index \
    .

# Clean up
echo "Cleaning up..."
docker rm -f postgres-brc20-build
docker network rm brc20-build-network

echo "Build complete!"