#!/bin/sh
# Creates one database per module named in the environment (the .env the
# postgres service loads). Runs once, when the postgres volume is empty.
set -eu

for name in "${DB_DATABASE:-}" "${BITMAP_DB_DATABASE:-}" "${SNS_DB_DATABASE:-}" "${RUNES_DB_DATABASE:-}"; do
    if [ -n "$name" ] && [ "$name" != "${POSTGRES_DB:-postgres}" ]; then
        echo "creating database $name"
        createdb --username "$POSTGRES_USER" "$name"
    fi
done
