#!/bin/bash

# This script prepares sqlx offline data for Docker builds
# It requires DATABASE_URL to be set when run locally

if [ -z "$DATABASE_URL" ]; then
    echo "Error: DATABASE_URL is not set"
    echo "Please set DATABASE_URL before running this script"
    exit 1
fi

echo "Preparing sqlx offline data..."
CXXFLAGS="-I$(xcrun --show-sdk-path)/usr/include/c++/v1" cargo sqlx prepare

if [ $? -eq 0 ]; then
    echo "Successfully generated .sqlx/query-*.json files"
else
    echo "Failed to prepare sqlx data"
    exit 1
fi