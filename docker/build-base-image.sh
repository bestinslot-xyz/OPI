#!/bin/bash

# Build the base image with pre-compiled RocksDB targeting amd64 architecture
docker buildx build --platform linux/amd64 -f rust-rocksdb-base.Dockerfile -t registry.bestinslot.xyz/rust-rocksdb-base:0.0.1 --push .

echo "Base image 'rust-rocksdb-base:latest' built successfully!"
echo "This image contains pre-compiled RocksDB and common dependencies."
echo ""
echo "To use this base image in your projects, update your Dockerfiles to:"
echo "FROM rust-rocksdb-base:latest as builder"
