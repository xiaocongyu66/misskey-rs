#!/usr/bin/env bash
# Generates mk-db/migrations/0001_init.sql by running the FULL upstream
# TypeORM migration chain (349 migrations at the pinned SHA) against a
# scratch PostgreSQL, then dumping the resulting schema.
#
# Intended to run in CI (see .github/workflows/gen-baseline.yml). Requires:
#   - upstream checkout at ./upstream, fully built (pnpm install + build)
#   - PG_* env for a database where migrations may run (DB IS DROPPED)
set -euo pipefail

: "${PGHOST:=127.0.0.1}" "${PGPORT:=5432}" "${PGUSER:=postgres}" "${PGPASSWORD:=postgres}" "${PGDATABASE:=misskey_baseline}"

psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d postgres -c "DROP DATABASE IF EXISTS $PGDATABASE;" -c "CREATE DATABASE $PGDATABASE;"

mkdir -p upstream/.config
cat > upstream/.config/default.yml <<EOF
url: http://127.0.0.1:3000/
port: 3000
db:
  host: $PGHOST
  port: $PGPORT
  db: $PGDATABASE
  user: $PGUSER
  pass: $PGPASSWORD
redis:
  host: 127.0.0.1
  port: 6379
id: aid
EOF

cd upstream
pnpm --filter backend compile-config
pnpm --filter backend migrate

pg_dump --schema-only --no-owner --no-privileges -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" "$PGDATABASE" > ../crates/mk-db/migrations/0001_init.sql
echo "schema lines: $(wc -l < ../crates/mk-db/migrations/0001_init.sql)"
