# Dependency matrix: upstream `packages/backend/test/e2e/users.ts`

Source: `/tmp/misskey/packages/backend/test/e2e/users.ts` (898 lines, upstream AGPL-3.0 suite).
Target: `/root/misskey-rs` Rust server, current endpoint set per `crates/misskey-rs/src/api/mod.rs`
(M0: `ping`, `meta`, `endpoints`, `stats`, `server-info`, `get-online-users-count`, `emojis` +
special-cases `signup`, `signin`, `users/show`, `_test/reset`).

All tests live under the single root describe **`ユーザー`** ("users"). Full names below are
written as `ユーザー > <test name>` (Japanese, verbatim).

## Totals

- 179 executed `test(...)`/`test.each(...)` cases (each-case counts expanded), plus
  6 `test.todo(...)` (not executed) and 1 `test.skip(...)`.
- Distinct endpoints invoked: **24** (incl. 2 via helpers, 1 multipart route).
- Rust already serves **2** of them (`signup`, `users/show`). None of the M0 read
  endpoints are used by this suite.

## Global hook gates (apply to every test)

| Hook | Endpoints | Infra | Notes |
|---|---|---|---|
| `beforeAll` (10 min timeout) | `signup` x20, `notes/create` (via `post`) x~30, `i/update` x8, `admin/roles/create` (via `role()`) x5, `admin/roles/assign` x5, `admin/suspend-user`, `i/delete-account`, `admin/delete-account`, `following/create` x3, `blocking/create` x2, `mute/create` x2, `renote-mute/create` x2 | admin/roles for role users | Hard gate: vitest fails the whole file if `beforeAll` throws. Rust harness must either implement all of these or re-seed fixtures directly in DB. |
| `beforeEach` | `i`, `notes/show` | — | Runs before **every** one of the 179 cases and asserts success. `i` is effectively the #1 gate for the entire file. |

Infra flags needed anywhere in the file: `uploadFile` (multipart `drive/files/create` + drive
storage) — Avatar/Banner tests only; `role()` (`admin/roles/create` + assign). **No websocket
streaming and no webhook infra is used in users.ts.**

## Endpoint inventory vs Rust status

| Endpoint | Used by | Rust status |
|---|---|---|
| `signup` | signup test + all beforeAll fixtures | EXISTS (special route) |
| `users/show` | 100+ assertions, `show()` helper, beforeAll memo/role checks | EXISTS |
| `i` | beforeEach, 1 direct test | missing |
| `i/update` | 62-case each, Avatar, Banner, hashtags, beforeAll fixtures | missing |
| `i/pin`, `i/unpin` | pin test | missing |
| `notes/create` | `post()` helper (beforeAll, frq tests, aliceNote/bobNote) | missing |
| `notes/show` | beforeEach | missing |
| `users` | list tests (16 cases) | missing |
| `users/update-memo` | memo tests (3 cases) | missing |
| `users/search` | search tests (11 cases) | missing |
| `users/search-by-username-and-host` | 18 cases | missing |
| `users/get-frequently-replied-users` | 9 cases | missing |
| `users/notes` | frq exclusion tests (fixture step) | missing |
| `hashtags/users` | hashtag tests (15 cases) | missing |
| `pinned-users` | 1 case (gated by `admin/update-meta`) | missing |
| `i/delete-account` | beforeAll (userDeletedBySelf fixture) | missing |
| `following/create`, `blocking/create`, `mute/create`, `renote-mute/create` | beforeAll relation fixtures | missing |
| `drive/files/create` (via `uploadFile`) | Avatar, Banner | BLOCKED (multipart/drive) |
| `admin/roles/create`+`assign` (via `role()`) | beforeAll (5 role users) | BLOCKED (admin/roles) |
| `admin/suspend-user`, `admin/delete-account`, `admin/update-meta` | beforeAll + pinned-users | BLOCKED (admin) |
| Todos only: `admin/show-user`, `admin/show-users`, `federation/users` | test.todo | not executed |

## Per-test matrix

Status legend — PASS-EXPECTED: body only touches existing endpoints (`signup`/`users/show`).
READY-LATER: body needs only user-facing (non-admin, non-drive) endpoints, listed.
BLOCKED: needs admin/roles, admin/*, or drive upload (in body **or** required fixture; noted).
Note: beforeAll/beforeEach hook requirements are global (see gates above); "fixture" notes below
are the *additional* per-test data dependencies.

| Test (`ユーザー > …`) | Cases | Endpoints | Infra | Status |
|---|---|---|---|---|
| `が作れる。（作りたての状態で自分のユーザー情報が取れる）` | 1 | `signup` | — | PASS-EXPECTED |
| `を読み取ることができること（自分）、キーが過不足なく入っていること。` | 1 | `i` | — | READY-LATER: `i` |
| `を書き換えることができる($#)` (test.each: name/description/location/fields/flags/visibility/mutedWords/…) | 62 | `i/update` | — | READY-LATER: `i/update` |
| `を書き換えることができる(Avatar)` | 1 | `uploadFile`, `i/update` | drive | BLOCKED (drive) |
| `を書き換えることができる(Banner)` | 1 | `uploadFile`, `i/update` | drive | BLOCKED (drive) |
| `を書き換えることができる(ピン止めノート)` | 1 | `i/pin`, `i/unpin` | fixture: `notes/create` (aliceNote) | READY-LATER: `i/pin`, `i/unpin`, `notes/create` |
| `を書き換えることができる(メモを$labelに)` (最大長/空文字/null) | 3 | `users/update-memo`, `users/show` | — | READY-LATER: `users/update-memo` |
| `をリスト形式で取得することができる（$label）` (ID昇順, follower±, createdAt±, updatedAt±) | 7 | `users`, `users/show` | — | READY-LATER: `users` |
| `をリスト形式で取得することができ、結果に$label` | 9 | `users`, `users/show` | fixtures: `i/update` x3, other-user `mute/create`/`blocking/create`, `admin/roles` x1, `admin/suspend-user`, `i/delete-account`, `admin/delete-account` | 6 READY-LATER (`users` + social/i-update fixtures: social pair for muted/blocked cases), 3 BLOCKED (silenced via role, suspended, deleted-by-admin) |
| `を取得することができる（$label）` (users/show ID/@ x self/other/unauth) | 6 | `users/show` | relations for "他人" cases seeded in beforeAll | PASS-EXPECTED (body); full `withRelations` keys need social fixtures |
| `を取得することができ、$labelこと` (admin/mod/2FA/silence/deleted/relation/pending flags) | 20 | `users/show` | fixtures: `admin/roles` x3 (admin, mod, silence), `i/delete-account` x2, `admin/delete-account` x2, social/mute/rn-mute/pending-follow fixtures | body PASS-EXPECTED; 7 cases BLOCKED at fixture (admin/roles or admin delete), 4 more need `i/delete-account`, 9 need social endpoints |
| `を取得することができ、Publicなロールがセットされていること` | 1 | `users/show` | fixture: `admin/roles/assign` | BLOCKED (fixture: admin/roles) |
| `を取得することができ、バッヂロールがセットされていること` | 1 | `users/show` | fixture: `admin/roles/assign` | BLOCKED (fixture: admin/roles) |
| `をID指定のリスト形式で取得することができる（空）` | 1 | `users/show` (`userIds: []`) | — | PASS-EXPECTED |
| `をID指定のリスト形式で取得することができる` | 1 | `users/show` (`userIds`) | — | PASS-EXPECTED |
| `をID指定のリスト形式で取得することができ、結果に$label` | 9 | `users/show` (`userIds`) | same fixture spread as users-list exclusions | 6 READY-LATER fixture-wise, 3 BLOCKED (silenced/suspended/deleted-by-admin) |
| `を検索することができる` / `を検索することができる(UserLite)` | 2 | `users/search`, `users/show` | — | READY-LATER: `users/search` |
| `を検索することができ、結果に$labelが含まれる` | 9 | `users/search`, `users/show` | fixtures as above | 6 READY-LATER, 3 BLOCKED |
| `をID&ホスト指定で検索できる($label)` | 9 | `users/search-by-username-and-host`, `users/show` | fixture: `following/create` (followed local user w/ note) | READY-LATER (+ social fixture) |
| `をID&ホスト指定で検索でき、結果に$label` | 9 | `users/search-by-username-and-host`, `users/show` | as above | 6 READY-LATER, 3 BLOCKED |
| `がよくリプライをするユーザーのリストを取得できる` | 1 | `users/get-frequently-replied-users`, `users/show`, `notes/create` (beforeAll replies) | — | READY-LATER: `users/get-frequently-replied-users`, `notes/create` |
| `がよくリプライをするユーザーのリストを取得でき、結果に$label` | 8 | `users/get-frequently-replied-users`, `users/notes`, `posting via notes/create`, `users/show` | fixtures incl. `admin/roles` (silence), `admin/delete-account` | 6 READY-LATER, 2 BLOCKED |
| `をハッシュタグ指定で取得することができる($label)` (sorts) | 6 | `i/update` (description tag), `hashtags/users`, `users/show` | — | READY-LATER: `hashtags/users`, `i/update` |
| `をハッシュタグ指定で取得することができ、結果に$label` | 9 | `i/update`, `hashtags/users`, `users/show` | fixtures as above | 6 READY-LATER, 3 BLOCKED |
| `のオススメを取得することができる` (`users/recommendation`) | 1 | `users/recommendation` | — | SKIPPED upstream (`test.skip`, known upstream SQL bug) |
| `のピン止めユーザーを取得することができる` | 1 | `admin/update-meta`, `pinned-users`, `users/show` | admin | BLOCKED (`admin/update-meta`) |
| todos: `を管理人として確認することができる(admin/show-user)` / `(admin/show-users)` / `をサーバー向けに取得することができる(federation/users)` | 3 | `admin/show-user`, `admin/show-users`, `federation/users` | admin | TODO (not executed) |

## Status roll-up

| Classification | Cases | Notes |
|---|---|---|
| PASS-EXPECTED (body) | 40 | signup(1) + users/show suite(39); beforeAll/beforeEach hooks still need `i`, `notes/show`, `notes/create` or a seeded harness; 7 of the 40 additionally need admin/role fixtures for full assertions |
| READY-LATER (body-only, no admin/drive) | ~124 | dominated by `i/update` (62), users/list (16), search-by-username (18), hashtags (15), users/search (11), frq (8+1), memo (3), pin (1) |
| BLOCKED | 15 | 2 drive (Avatar/Banner) + 13 admin-fixture/admin-endpoint cases |
| Skipped / TODO | 4 | 1 skip + 3 todo |

## Prioritized "next 10 endpoints"

Ranked by number of users.ts cases each unblocks (after `signup`/`users/show` which exist):

1. **`i`** — beforeEach gate for all 179 cases + 1 direct test. Nothing in this file is runnable end-to-end without it.
2. **`i/update`** — 62 direct cases + 15 hashtag cases + beforeAll fixture editing (~30 exclusion-assert cases). Largest single unlock.
3. **`notes/create`** (`post()` helper) — every fixture with a note; required for pin, frq, and relation tests; aliceNote is asserted in beforeEach via `notes/show` too.
4. **`notes/show`** — beforeEach gate (runs 179x).
5. **`users/search-by-username-and-host`** — 18 cases, body trivial once `users/show`-style packing exists.
6. **`users`** — 16 cases.
7. **`hashtags/users`** — 15 cases (pairs with `i/update`).
8. **`users/search`** — 11 cases.
9. **`users/notes`** — fixture step for the 8 frq exclusion cases (also generally useful).
10. **`users/get-frequently-replied-users`** — 9 cases.

Cheap runners-up: `users/update-memo` (3 cases, tiny), `i/pin` + `i/unpin` (1 case, tiny).
Admin/roles and drive unblock the remaining 15 BLOCKED cases but unlock nothing else in this
file relative to their cost — defer.
