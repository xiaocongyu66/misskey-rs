# misskey-rs — a Rust axum+sqlx rewrite of Misskey's backend

[![CI](https://github.com/xiaocongyu66/misskey-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/xiaocongyu66/misskey-rs/actions/workflows/ci.yml)

A Rust rewrite of the [Misskey](https://github.com/misskey-dev/misskey) backend, tracking
upstream `misskey-dev/misskey` @ `ea2c3aed`. Client-compatible: clients keep POSTing
`/api/<endpoint>` with the `i` token; response shapes, error codes and the streaming
protocol mirror upstream. The upstream e2e suites (`packages/backend/test/e2e/`) are the
acceptance spec — CI runs them directly against this server.

The API layer serves **438 upstream endpoint names registered** (`crates/misskey-rs/src/api/endpoints_catalog.rs`,
mirroring upstream `endpoints.ts` order, incl. `admin/*`). The database schema is generated
from the upstream TypeORM migration chain (`gen-baseline.yml`), so a diff against a real
instance is the compatibility target.

## Workspace layout

| crate | role |
|---|---|
| `misskey-rs` | the server binary — axum API, WebSocket streaming, web/file routes (`bin/misskey-rs`) |
| `mk-db` | PostgreSQL access: pool construction + migrations — the only crate allowed to touch SQL |
| `mk-core` | business services ported from upstream `core/` (NoteCreate, entity packers, MetaService, FanoutTimeline, RoleService/policies, Drive, charts, …) |
| `mk-queue` | PG-backed durable job queue (`mk_queue_jobs`, `FOR UPDATE SKIP LOCKED`) + in-process worker — replaces upstream BullMQ |
| `mk-ap` | ActivityPub: renderer, resolver, signed HTTP (draft-cavage-11), inbox processing, audience resolution |
| `mk-types` | shared dependency-light types: ID generators (aid), the API error envelope |
| `mk-jsonschema` | Ajv-parity validation of Misskey `paramDef` JSON Schemas (`nullable`/`optional`/`useDefaults`/`misskey:id`) |
| `mfm-rs` | Rust port of the MFM parser (`mfm-js`); CI runs a differential corpus against the TS implementation |

## Quickstart

```sh
docker run -d --name misskey-pg  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=misskey -p 5432:5432 postgres:16
docker run -d --name misskey-redis -p 6379:6379 redis:7-alpine
```

Config shape mirrors upstream `config.ts` — unknown keys are ignored, so a real upstream
config loads as-is (sample: `config/config.example.yaml`; the server reads `./config.yaml`
or the path given as the first argument, see `crates/misskey-rs/src/config.rs`):

```yaml
url: http://localhost:3000
port: 3000

db:
  host: 127.0.0.1
  port: 5432
  user: postgres
  pass: postgres
  db: misskey

redis:
  host: 127.0.0.1
  port: 6379
```

```sh
cargo run --release --bin misskey-rs
curl -s http://localhost:3000/healthz
```

## Verification matrix — upstream e2e arenas

Every upstream e2e suite gets its own CI lane (`.github/workflows/e2e-*.yml`) that runs the
pinned upstream test file against this server. Green below means a **log-verified vitest
pass** of that suite — not the workflow run conclusion (several lanes tolerate failures at
the step level, so a green ✅ on GitHub is not evidence by itself). `X/X` counts executed
tests; upstream `skip`/`todo` rows are excluded.

| arena (upstream `test/e2e/`) | result | latest green run |
|---|---|---|
| api.ts | 20/20 | 39245061028 |
| api-visibility.ts | 78/78 | 39235207874 |
| announcements.ts | 2/2 | 39143799573 |
| antennas.ts | 87/87 | 39240737356 |
| block.ts | 6/6 | 39204310430 |
| channels.ts | 1/1 | 39128111258 |
| clips.ts | 97/97 | 39215321870 |
| endpoints.ts | 92/92 | 39224106216 |
| exports.ts | 5/5 | 39298609580 |
| fetch-resource.ts | 84/84 | 39322935325 |
| ff-visibility.ts | 20/20 | 39218656794 |
| files.ts | 8/8 | 39314476216 |
| follow-approval.ts | 18/18 | 39240377087 |
| nodeinfo.ts | 2/2 | 39230498208 |
| note-notify.ts | 8/8 | 39240720240 |
| note.ts | 52/52 | 39270107968 |
| oauth.ts | 52/52 | 39289171046 |
| reversi-game.ts | 1/1 | 39319528070 |
| roles.ts | 14/14 | 39299042259 |
| streaming.ts | 53/53 | 39247101579 |
| timelines.ts | 378/378 | 39179590872 |
| users.ts | 179/179 | 39118917713 |
| well-known.ts | 5/5 | 39215008064 |

In-flight arenas (not yet cleared, latest logged result):

| arena | state | reference run |
|---|---|---|
| 2fa.ts | 3 passed / 4 failed of 7 | 39300653792 |
| drive.ts | 3 passed / 1 failed of 4 (later runs died pre-vitest) | 39128302999 |
| move.ts | 59 passed / 3 failed of 62 (follow-ups died pre-vitest) | 39322949913 |
| `{user-notes,renote-mute,thread-mute,mute}.ts` (batch-social lane) | 27 passed / 2 failed of 29 (best) | 39199419722 |

Per-arena census notes (fix history, error-ladder provenance, ×2 confirmations) live in
[`e2e-harness/census.md`](e2e-harness/census.md) and the CI lane logs
([Actions](https://github.com/xiaocongyu66/misskey-rs/actions)).

## License

AGPL-3.0-only (same as upstream Misskey).
