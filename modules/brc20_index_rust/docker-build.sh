#!/bin/bash
set -e

echo "Starting PostgreSQL container for build..."

# Start PostgreSQL container
docker run -d \
    --name postgres-brc20-build \
    -e POSTGRES_USER=postgres \
    -e POSTGRES_PASSWORD=postgres \
    -e POSTGRES_DB=brc20_index \
    -p 54322:5432 \
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
        exit 1
    fi
    sleep 1
done

# Initialize database schema
echo "Initializing database schema..."
docker exec -i postgres-brc20-build psql -U postgres -d brc20_index < src/database/sql/db_init.sql

# Build the Docker image with DATABASE_URL
echo "Building Docker image..."
docker build \
    --build-arg DATABASE_URL="postgres://postgres:postgres@host.docker.internal:54322/brc20_index" \
    -t brc20-index \
    .

# Clean up PostgreSQL container
echo "Cleaning up PostgreSQL container..."
docker rm -f postgres-brc20-build

echo "Build complete!"