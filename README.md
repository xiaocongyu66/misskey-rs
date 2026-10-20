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

In-flight arenas (not yet cleared, latest logged result): none — every arena is green.

Late corrections (audit-verified; the matrix above holds their final numbers):
drive.ts 4/4 (rn39130643907), batch-social 29/29 (rn39202308911), move.ts 62/62 ×2
(rn39326293175 + rn39327166962 @ bcb660f), 2fa.ts 7/7 (rn39343393617 @ 7670e65).
Extension lanes beyond the 29-file pinned universe: files.ts 8/8, roles.ts 14/14,
synalio 11/11, fetch-validate 2/2 ×2.

Per-arena census notes (fix history, error-ladder provenance, ×2 confirmations) live in
[`e2e-harness/census.md`](e2e-harness/census.md) and the CI lane logs
([Actions](https://github.com/xiaocongyu66/misskey-rs/actions)).

## Coverage scope (read before comparing to upstream)

The green matrix above is scoped to the surfaces the pinned e2e suite exercises:
the API dispatch layer (438 upstream endpoint names registered, paramDef/error-id
parity), streaming (7 channels + Redis fanout), auth (TOTP, WebAuthn, OAuth2 PKCE),
the web front door (apOrHtml negotiation, feeds, SSR metas), account migration,
and the export-clips queue lane. Known substantive gaps vs upstream
(misskey @ ea2c3aed), each verified by absence of the symbol:

- **Federation transport** — skeleton landed: `mk-ap::signature` (HTTP-Signature
  parse/signing-string/RSA-SHA256 verify per `ap-request.ts` pins) and
  `mk-ap::inbox` (activity classify + 202-accept dispatch) are live on
  `POST /inbox` + `POST /users/:id/inbox`. Still missing: remote-key lookup by
  keyId, worker-side verify wiring, and the per-activity processors
  (Create{Note} upsert, Follow rows, etc.).
- **Queue processors** — `exportClips`, `endedPollNotification` (delayed enqueue
  on both poll-insert lanes, pollEnded notifications for local author+voters)
  and `deleteAccount` (soft slice: note purge + isDeleted) are ported onto the
  durable PG worker. Still missing: the Export*/Import* family beyond clips,
  email deliverer, Redis fanout on notification insert.
- **Web push and email** — not implemented (no `web_push`/`sendEmail` symbols).
- **Meilisearch** — `notes/search` + `notes/search-by-tag` endpoints landed with
  the upstream no-Meili semantics (SQL ILIKE fallback, `UNAVAILABLE`
  0b44998d gate, host `.` sentinel, suspended-author filter). Still missing:
  index writes on note create/delete and Meili-backed query paths.
- **Charts / stats aggregation** — absent; nodeinfo counts are direct queries.
- **MFM rendering** — a real parser/renderer lives in `mfm-rs` (Text/Bold/Italic/
  Strike/Small/InlineCode with upstream escape parity, strike→`<del>`), and
  feeds use it. Still missing: the remaining mfm-js node set (mentions,
  hashtags, custom emoji, quote/center/code-block, ruby, fn).

These are roadmap items, not regressions: every claim in the verification matrix is
backed by a GitHub Actions run log, and nothing in the table asserts federation,
push, or mail behavior.

## License

AGPL-3.0-only (same as upstream Misskey).
