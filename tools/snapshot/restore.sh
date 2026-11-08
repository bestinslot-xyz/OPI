#!/usr/bin/env bash
# Restore an OPI snapshot (https://snap.opi.network) into the volumes of
# docker/compose.yml, so the node starts at the snapshot's height instead of
# indexing from the start. Run from anywhere in the repository, after `.env` is
# written and the image is built, and before the stack first starts:
#
#   tools/snapshot/restore.sh                    newest pruned-journal snapshot for OPI_CHAIN in .env
#   OPI_JOURNAL=full tools/snapshot/restore.sh   newest full-journal snapshot
#   tools/snapshot/restore.sh mainnet/975052/    a given snapshot, relative to $SNAPSHOT_BASE
#   tools/snapshot/restore.sh /path/to/dir       a snapshot directory already on disk
#
# Downloads land in $DOWNLOAD_DIR (default ./snapshot-download) and are removed
# after a good restore unless KEEP_DOWNLOAD=1. Every file is checked against
# SHA256SUMS. The snapshot's database role carries a placeholder password; the
# restore gives DB_USER (and BITMAP_DB_USER, SNS_DB_USER when set) the
# passwords from .env and disables the placeholder login.
set -euo pipefail

cd "$(dirname "$0")/../.."
SNAPSHOT_BASE=${SNAPSHOT_BASE:-https://cdn.opi.network/snapshots}
OPI_JOURNAL=${OPI_JOURNAL:-pruned}
DOWNLOAD_DIR=${DOWNLOAD_DIR:-./snapshot-download}
KEEP_DOWNLOAD=${KEEP_DOWNLOAD:-0}
FILES="opi-data.tar.zst postgres.tar.zst"

log() { echo "$(date -u +%FT%TZ) $*"; }
die() { echo "$*" >&2; exit 1; }
dc() { docker compose -f docker/compose.yml "$@"; }
# A value from .env, without surrounding quotes.
env_get() { sed -n "s/^$1=//p" .env | tail -1 | sed -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/"; }
json_get() { sed -n "s/.*\"$2\": *\"\{0,1\}\([^\",]*\)\"\{0,1\}.*/\1/p" "$1" | head -1; }

for c in curl zstd docker; do command -v $c >/dev/null || die "$c is not installed"; done
if command -v sha256sum >/dev/null; then sha() { sha256sum -c SHA256SUMS; }; else sha() { shasum -a 256 -c SHA256SUMS; }; fi
[ -f .env ] || die "write .env first (cp .env_sample .env, then edit it)"
DB_USER=$(env_get DB_USER); DB_USER=${DB_USER:-postgres}
[ -n "$(env_get DB_PASSWD)" ] || die "set DB_PASSWD in .env"
chain=$(env_get OPI_CHAIN); chain=${chain:-mainnet}
case "$OPI_JOURNAL" in pruned) latest=latest.json ;; full) latest="latest-full.json" ;;
  *) die "OPI_JOURNAL is pruned or full" ;; esac

# --- the volumes must be fresh ---
dc create --no-recreate postgres indexer >/dev/null
[ -z "$(dc run --rm --no-deps -T --user 0 --entrypoint sh indexer -c 'find /data -mindepth 2 | head -1')" ] \
  || die "the opi-data volume is not empty; remove it first (docker compose -f docker/compose.yml down -v)"
[ -z "$(dc run --rm --no-deps -T --user 0 --entrypoint sh postgres -c 'ls -A /var/lib/postgresql/data')" ] \
  || die "the postgres-data volume is not empty; remove it first (docker compose -f docker/compose.yml down -v)"

# --- fetch and verify ---
if [ -d "${1:-}" ]; then
  src=${1%/}
else
  if [ -n "${1:-}" ]; then
    path=${1%/}/
  else
    path=$(curl -fsSL "$SNAPSHOT_BASE/$chain/$latest" | sed -n 's/.*"path": *"\([^"]*\)".*/\1/p')
    [ -n "$path" ] || die "no snapshot listed at $SNAPSHOT_BASE/$chain/$latest"
  fi
  src=$DOWNLOAD_DIR
  mkdir -p "$src"
  for f in manifest.json SHA256SUMS $FILES; do
    log "downloading $SNAPSHOT_BASE/$path$f"
    curl -fL --retry 10 --retry-delay 10 -C - -o "$src/$f" "$SNAPSHOT_BASE/$path$f"
  done
fi
log "verifying checksums"
(cd "$src" && sha)
[ "$(json_get "$src/manifest.json" chain)" = "$chain" ] \
  || die "the snapshot is for $(json_get "$src/manifest.json" chain), .env says OPI_CHAIN=$chain"
snap_user=$(json_get "$src/manifest.json" postgres_user)
height=$(sed -n 's/.*"block_height": *\([0-9]*\).*/\1/p' "$src/manifest.json")

# --- extract into the volumes ---
log "extracting opi-data.tar.zst into the opi-data volume"
zstd -dc "$src/opi-data.tar.zst" \
  | dc run --rm --no-deps -T --user 0 --entrypoint tar indexer -x --numeric-owner --strip-components=1 -C /data
dc run --rm --no-deps -T --user 0 --entrypoint chown indexer -R 10001:10001 /data
log "extracting postgres.tar.zst into the postgres-data volume"
zstd -dc "$src/postgres.tar.zst" \
  | dc run --rm --no-deps -T --user 0 --entrypoint tar postgres -x --numeric-owner -p --strip-components=2 -C /var/lib/postgresql/data
dc run --rm --no-deps -T --user 0 --entrypoint chown postgres -R postgres:postgres /var/lib/postgresql/data

# --- database roles ---
log "starting postgres to set the database passwords"
dc up -d --wait postgres
# A value as a single-quoted psql meta-command argument.
pq() { local s=${1//\\/\\\\}; s=${s//\'/\'\'}; printf "'%s'" "$s"; }
users=" "
role() {  # USER PASSWORD: create the role if missing and set its password
  [ -n "$1" ] && [ -n "$2" ] || return 0
  case "$users" in *" $1 "*) return 0 ;; esac
  users="$users$1 "
  printf '\\set u %s\n\\set pw %s\n' "$(pq "$1")" "$(pq "$2")"
  printf "SELECT format('CREATE ROLE %%I SUPERUSER LOGIN', :'u') WHERE NOT EXISTS (SELECT FROM pg_roles WHERE rolname = :'u') \\\\gexec\n"
  printf "SELECT format('ALTER ROLE %%I PASSWORD %%L', :'u', :'pw') \\\\gexec\n"
}
{ role "$DB_USER" "$(env_get DB_PASSWD)"
  role "$(env_get BITMAP_DB_USER)" "$(env_get BITMAP_DB_PASSWD)"
  role "$(env_get SNS_DB_USER)" "$(env_get SNS_DB_PASSWD)"
  case "$users" in *" $snap_user "*) ;; *)
    printf '\\set u %s\n' "$(pq "$snap_user")"
    printf "SELECT format('ALTER ROLE %%I NOLOGIN PASSWORD NULL', :'u') \\\\gexec\n" ;;
  esac
} | dc exec -T postgres psql -X -q -v ON_ERROR_STOP=1 -U "$snap_user" -d postgres
dc stop postgres

[ "$KEEP_DOWNLOAD" = 1 ] || [ "$src" != "$DOWNLOAD_DIR" ] || rm -rf "$DOWNLOAD_DIR"
log "restored $chain at block $height"
echo "start the node: docker compose -f docker/compose.yml up -d"
