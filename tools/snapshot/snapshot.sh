#!/usr/bin/env bash
# Pack a snapshot of a node run with docker/compose.yml: the opi-data volume
# (ord index, journal, BRC2.0 engine) and the postgres-data volume, both at the
# same block, in the layout restore.sh reads. Run from anywhere in the
# repository on a node at the tip:
#
#   tools/snapshot/snapshot.sh [OUT_DIR]        default ./snapshots
#
# The indexer, the API and postgres stop while the archives are written. The
# archives are meant to be shared: before postgres stops, DB_USER's password is
# set to the placeholder `opi`, the catalog rows and WAL that held the old one
# are flushed out, and only the one WAL segment a clean start needs is packed.
# On exit, success or failure, DB_USER gets DB_PASSWD from .env back and the
# stack starts again.
#
# Output: OUT_DIR/<chain>-<height>[-full]/{opi-data.tar.zst,postgres.tar.zst,manifest.json,SHA256SUMS}
set -euo pipefail

cd "$(dirname "$0")/../.."
OUT_ROOT=${1:-./snapshots}
ZSTD_LEVEL=${ZSTD_LEVEL:-6}
PLACEHOLDER_PASSWORD=opi
API=${API:-http://127.0.0.1:8000}

log() { echo "$(date -u +%FT%TZ) $*"; }
die() { echo "$*" >&2; exit 1; }
dc() { docker compose -f docker/compose.yml "$@"; }
env_get() { sed -n "s/^$1=//p" .env | tail -1 | sed -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'$/\1/"; }

for c in curl zstd docker; do command -v $c >/dev/null || die "$c is not installed"; done
if command -v sha256sum >/dev/null; then sums() { sha256sum "$@"; }; else sums() { shasum -a 256 "$@"; }; fi
DB_USER=$(env_get DB_USER); DB_USER=${DB_USER:-postgres}
[ -n "$(env_get DB_PASSWD)" ] || die "DB_PASSWD is not set in .env"
chain=$(env_get OPI_CHAIN); chain=${chain:-mainnet}
db() { env_get "$1"; }
brc20_db=$(db DB_DATABASE); brc20_db=${brc20_db:-opi_brc20}
modules=$(env_get OPI_MODULES); modules=${modules:-brc20}

psql_su() { dc exec -T postgres psql -X -q -v ON_ERROR_STOP=1 -U "$DB_USER" "$@"; }
# A value as a single-quoted psql meta-command argument.
pq() { local s=${1//\\/\\\\}; s=${s//\'/\'\'}; printf "'%s'" "$s"; }
set_password() {  # the value reaches psql on stdin, never argv
  { printf '\\set u %s\n\\set pw %s\n' "$(pq "$DB_USER")" "$(pq "$1")"
    printf "SELECT format('ALTER ROLE %%I PASSWORD %%L', :'u', :'pw') \\\\gexec\n"; } | psql_su -d postgres
}

# The archive carries the whole postgres cluster, so it holds only the module
# databases and no password but DB_USER's, which the pack replaces.
module_dbs="'postgres'"
for v in DB_DATABASE BITMAP_DB_DATABASE SNS_DB_DATABASE RUNES_DB_DATABASE INSCRIPTIONS_DB_DATABASE; do
  n=$(env_get $v); [ -z "$n" ] || module_dbs="$module_dbs, $(pq "$n")"
done
extra_dbs=$(psql_su -d postgres -At -c "select string_agg(datname, ' ') from pg_database where not datistemplate and datname not in ($module_dbs)")
[ -z "$extra_dbs" ] || die "the postgres cluster holds databases outside OPI_MODULES ($extra_dbs); the archive would carry them"
other_pw=$(printf '\\set u %s\nselect string_agg(rolname, '"' '"') from pg_authid where rolpassword is not null and rolname <> :'"'u'"';\n' "$(pq "$DB_USER")" \
  | psql_su -d postgres -At)
[ -z "$other_pw" ] || die "roles other than DB_USER have passwords ($other_pw); the archive would carry them"

stopped=false
bring_back() {
  $stopped || return 0
  log "starting postgres, restoring DB_USER's password, starting the stack"
  dc up -d --wait postgres
  set_password "$(env_get DB_PASSWD)"
  dc up -d
  stopped=false
}
trap bring_back EXIT

log "stopping the indexer (waits for the block in flight)"
stopped=true
dc stop indexer

height=$(psql_su -d "$brc20_db" -At -c 'select max(block_height) from brc20_block_hashes')
hashes=$(curl -fsS "$API/v1/brc20/get_hash_of_all_activity?block_height=$height")
field() { printf '%s' "$hashes" | sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }
event_hash=$(field cumulative_event_hash) trace_hash=$(field cumulative_trace_hash)
indexer_version=$(field indexer_version)
[ -n "$height" ] && [ -n "$event_hash" ] || die "could not read the brc20 height and hash"
log "brc20 at $height, cumulative event hash $event_hash"

meta=$(dc run --rm --no-deps -T --user 0 --entrypoint cat indexer /data/journal/meta.json)
pruned_below=$(printf '%s' "$meta" | sed -n 's/.*"pruned_below": *\([0-9]*\).*/\1/p')
if [ -n "$pruned_below" ] && [ "$pruned_below" -gt 0 ]; then journal=pruned suffix=; else journal=full suffix=-full pruned_below=null; fi
commit=$(git rev-parse --short=7 HEAD 2>/dev/null || echo unknown)
pg_image=$(docker inspect -f '{{.Config.Image}}' "$(dc ps -q postgres)")

out="$OUT_ROOT/$chain-$height$suffix"
[ ! -e "$out" ] || die "$out already exists"
mkdir -p "$out"

dc stop api
log "setting the placeholder password and flushing the catalog"
psql_su -d postgres <<SQL
ALTER ROLE "$DB_USER" PASSWORD '$PLACEHOLDER_PASSWORD';
VACUUM FULL pg_catalog.pg_authid;
CHECKPOINT;
SELECT pg_switch_wal();
CHECKPOINT;
SQL
dc stop postgres

controldata=$(dc run --rm --no-deps -T --user postgres --entrypoint pg_controldata postgres /var/lib/postgresql/data)
printf '%s\n' "$controldata" | grep -q '^Database cluster state: *shut down$' || die "postgres did not shut down cleanly"
redo_wal=$(printf '%s\n' "$controldata" | sed -n "s/^Latest checkpoint's REDO WAL file: *//p")
[ -n "$redo_wal" ] || die "no REDO WAL file in pg_controldata"

log "packing opi data"
dc run --rm --no-deps -T --user 0 --entrypoint tar indexer --numeric-owner -C / --transform 's,^data,opi,' -cf - data \
  | zstd -q -T0 "-$ZSTD_LEVEL" -o "$out/opi-data.tar.zst"
# Everything outside pg_wal, pg_wal's directories and non-segment files, and the
# REDO segment. Older segments are spent and newer ones are recycled leftovers.
log "packing postgres (WAL: $redo_wal only)"
dc run --rm --no-deps -T --user 0 --entrypoint bash postgres -c '
  cd /var/lib/postgresql
  { find data -path data/pg_wal -prune -o -print0
    find data/pg_wal -regextype posix-extended ! -regex ".*/[0-9A-F]{24}" -print0
    printf "%s\0" "data/pg_wal/$1"
  } | tar --numeric-owner --null --no-recursion -T - --transform "s,^data,postgres/pgdata," -cf -' _ "$redo_wal" \
  | zstd -q -T0 "-$ZSTD_LEVEL" -o "$out/postgres.tar.zst"

bring_back

log "checksumming"
(cd "$out" && sums opi-data.tar.zst postgres.tar.zst > SHA256SUMS)
size_of() { wc -c < "$out/$1" | tr -d ' '; }
sum_of() { awk -v f="$1" '$2 == f {print $1}' "$out/SHA256SUMS"; }
mods=$(printf '"%s", ' $(printf '%s' "$modules" | tr ',' ' ')); mods=${mods%, }
cat > "$out/manifest.json" <<JSON
{
  "chain": "$chain",
  "modules": [$mods],
  "block_height": $height,
  "brc20_cumulative_event_hash": "$event_hash",
  "brc20_cumulative_trace_hash": "$trace_hash",
  "indexer_version": "$indexer_version",
  "journal": "$journal",
  "journal_pruned_below": $pruned_below,
  "opi_commit": "$commit",
  "postgres_image": "$pg_image",
  "postgres_user": "$DB_USER",
  "postgres_password": "$PLACEHOLDER_PASSWORD",
  "created_at": "$(date -u +%FT%TZ)",
  "files": [
    {"name": "opi-data.tar.zst", "extracts_to": "opi/", "size": $(size_of opi-data.tar.zst), "sha256": "$(sum_of opi-data.tar.zst)"},
    {"name": "postgres.tar.zst", "extracts_to": "postgres/", "size": $(size_of postgres.tar.zst), "sha256": "$(sum_of postgres.tar.zst)"}
  ]
}
JSON
log "done: $out"
