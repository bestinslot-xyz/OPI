#!/bin/bash

echo "Building lean Rust base image with RocksDB build dependencies..."
echo "This image will be much smaller (~400MB vs 1GB+) but still speed up builds significantly."

# Build the lean base image
docker buildx build --platform linux/amd64 \
    -f docker/rust-rocksdb-base-lean.Dockerfile \
    -t registry.bestinslot.xyz/rust-rocksdb-base:lean \
    . --load

echo ""
echo "Build complete! Image size:"
docker images registry.bestinslot.xyz/rust-rocksdb-base:lean

echo ""
echo "Pushing lean image to registry..."
docker push registry.bestinslot.xyz/rust-rocksdb-base:lean

echo ""
echo "✅ Lean base image pushed successfully!"
echo ""
echo "To use this base image in your projects:"
echo "FROM registry.bestinslot.xyz/rust-rocksdb-base:lean as builder"
echo ""
echo "Benefits:"
echo "- Much smaller image size (~400MB vs 1GB+)"
echo "- All build dependencies pre-installed"
echo "- Cargo registry pre-warmed with common dependencies"
echo "- Optimized build settings for faster compilation"
