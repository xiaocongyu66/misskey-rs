#!/usr/bin/env python3
"""Strip TypeORM bookkeeping table from the pg_dump baseline and install it
as the sqlx baseline migration."""

SRC = '/tmp/baseline-schema/0001_init.sql'
DST = '/root/misskey-rs/crates/mk-db/migrations/0001_init.sql'

out, skip = [], False
for line in open(SRC):
    line = line.rstrip('\n')
    if line.startswith('CREATE TABLE public.migrations ('):
        skip = True
        continue
    if skip:
        if line.startswith(');'):
            skip = False
        continue
    if 'public.migrations' in line:
        continue
    out.append(line)

open(DST, 'w').write('\n'.join(out) + '\n')
print('lines kept:', len(out))
print('migrations refs left:', sum(1 for l in out if 'migrations' in l))
