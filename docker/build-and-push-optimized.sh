#!/bin/bash

# Set Docker client timeout to 30 minutes (1800 seconds)
export DOCKER_CLIENT_TIMEOUT=1800
export COMPOSE_HTTP_TIMEOUT=1800

echo "Building optimized base image with pre-compiled RocksDB..."
echo "This may take a while due to compilation, but the resulting image will be smaller."

# Build the optimized base image targeting amd64 architecture
docker buildx build --platform linux/amd64 \
    -f docker/rust-rocksdb-base-optimized.Dockerfile \
    -t registry.bestinslot.xyz/rust-rocksdb-base:0.0.2 \
    . --load

echo "Base image built successfully!"
echo "Image size:"
docker images registry.bestinslot.xyz/rust-rocksdb-base:0.0.2

echo ""
echo "Pushing to registry with extended timeout..."
# Push with retry logic
for i in {1..3}; do
    echo "Push attempt $i/3..."
    if timeout 1800 docker push registry.bestinslot.xyz/rust-rocksdb-base:0.0.2; then
        echo "✅ Push successful!"
        break
    else
        echo "❌ Push attempt $i failed, retrying..."
        if [ $i -eq 3 ]; then
            echo "❌ All push attempts failed. Consider using docker save/load or reducing image size further."
            exit 1
        fi
        sleep 10
    fi
done
