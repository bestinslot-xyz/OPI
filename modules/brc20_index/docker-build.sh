#!/bin/bash
set -e

echo "Setting up Docker buildx..."

# Create or use existing buildx builder
BUILDER_NAME="brc20-builder"
if ! docker buildx ls | grep -q "$BUILDER_NAME"; then
    echo "Creating buildx builder..."
    docker buildx create --name "$BUILDER_NAME" --driver docker-container --use
else
    echo "Using existing buildx builder..."
    docker buildx use "$BUILDER_NAME"
fi

echo "Starting PostgreSQL container for build..."

# Start PostgreSQL container with exposed port
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

# Get the host IP for database connection
if [[ "$OSTYPE" == "darwin"* ]]; then
    # macOS - get the host IP that Docker Desktop uses
    HOST_IP=$(docker run --rm alpine getent hosts host.docker.internal | awk '{print $1}')
else
    # Linux - get the docker0 interface IP
    HOST_IP=$(ip -4 addr show docker0 | grep -oP '(?<=inet\s)\d+(\.\d+){3}' || echo "172.17.0.1")
fi

echo "Using host IP: ${HOST_IP}"

# Build the Docker image with buildx
echo "Building Docker image for linux/amd64..."
cp -R ../../ord/db_reader/ ./db_reader
docker buildx build \
    --platform linux/amd64 \
    --build-arg DATABASE_URL="postgres://postgres:postgres@${HOST_IP}:54322/brc20_index" \
    --add-host=postgres-host:${HOST_IP} \
    -t registry.bestinslot.xyz/brc20-index:0.0.16 \
    --push \
    .

# Clean up PostgreSQL container
echo "Cleaning up..."
docker rm -f postgres-brc20-build

echo "Build complete!"
