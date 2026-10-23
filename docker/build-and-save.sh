#!/bin/bash

# Alternative method for large images: save locally and transfer
echo "Building and saving large image locally..."

# Build the image
docker buildx build --platform linux/amd64 \
    -f docker/rust-rocksdb-base-optimized.Dockerfile \
    -t rust-rocksdb-base:0.0.2 \
    . --load

# Save to tar file
echo "Saving image to tar file..."
docker save rust-rocksdb-base:0.0.2 | gzip > rust-rocksdb-base-0.0.2.tar.gz

echo "Image saved as rust-rocksdb-base-0.0.2.tar.gz"
echo "You can transfer this file to your registry server and load it there with:"
echo "  scp rust-rocksdb-base-0.0.2.tar.gz user@registry.bestinslot.xyz:/tmp/"
echo "  ssh user@registry.bestinslot.xyz 'gunzip -c /tmp/rust-rocksdb-base-0.0.2.tar.gz | docker load'"
echo "  ssh user@registry.bestinslot.xyz 'docker tag rust-rocksdb-base:0.0.2 registry.bestinslot.xyz/rust-rocksdb-base:0.0.2'"
echo "  ssh user@registry.bestinslot.xyz 'docker push registry.bestinslot.xyz/rust-rocksdb-base:0.0.2'"
