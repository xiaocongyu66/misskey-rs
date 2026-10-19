latest: run 39217007189 — ff-visibility.ts CLEARED 20/20 (users/followers+following per-visibility gates + AP /users/:id/{followers,following} collections + admin/update-meta meta."federation" patch; persist arm must sit ABOVE the note/freq terminating arms — behind them the flop never landed and the AP public legs 403'd; ×2: rn39217007189 20/20 green + rn39218656794 confirm — census history: 39212045330 18/20 → 39213301960/39215956087 vitest 13s silent-deaths (fleet log-flake; well-known/endpoints cleared same window) → clear)
latest: run 39214163100 — well-known.ts CLEARED 5/5 (`.well-known/*` + `/nodeinfo/*` api-scoped cors, owned-extract E0277 Send fix; ×2 green: rn8 39214163100 + rn9 39215008064 — Test Files 1 passed (1) / Tests 5 passed (5))
latest: run 39179590872 — timelines.ts CLEARED 378/378 (keyPrefix parity)
# users.ts e2e census — misskey-rs（持续更新）

上游规格：misskey-dev/misskey @ ea2c3aed，`packages/backend/test/e2e/users.ts`
harness：`vitest.e2e-rs.config.ts` + `global-setup.mts`（spawn misskey-rs + /env{,-reset}）
运行日志聚类（第 8 轮，run 39064708934，commit 7bc556f~）：

```
Test Files  1 failed (1)
    Tests  100 failed | 79 passed | 1 skipped | 10 todo (190)
history note: run 39080371230 = 147 (nine-agent wave: notifications/social/admin/roles/queue + pack parity)
latest: run 39118917713 @ c03339c — 0 failed (179 passed + 1 skipped + 10 todo) USERSET CLEAR
history last-waves: 176 (gate-roles M3.5) → 179/0-final (delete-account soft flag + freq pack roles)
history: 0 → 17 (charset) → 70 (pack mirror) → 75 (i/update subset) → 79 (relations)
          → 93 (search) → 100 (hashtags+credits) → 112 (explore list) → 147 (M3 wave)```
```
（第 9-14 轮的线下教训：无真实关系数据时硬编码 relations=false 组会倒退 73 例；
followedMessage 仅 Me 视图。M2 用真实 getRelation + 条件展开一次性解决。）

## 关键解锁史
| 修复 | 收益 |
|---|---|
| harness include 目录/globalSetup 路径 | 集合执行 |
| /api/signup + users/show（M1/M1b） | 前置就绪 |
| `/api/notes/create + notes/show` 最小链 | beforeAll 可过（post() 依赖） |
| content-type `application/json; charset=utf-8`（e2e 严格串比较）| **0 → 17** |
| pack 镜像（isBot/isCat 在场、requireSignin/makeNotes 缺席、avatarBlurhash present-null）| **17 → 70** |
| i/update 补 show 家族持久化 + followedMessage + 5 个 jsonb 字段 | **70 → 75** |

## 剩余失败聚类（104，verifier 校正后的真实分桶）
- ~~`users/search` + search-by-username-and-host 族~~ 已实现（yield 14；剩余为 hashtags 与列表排序族）
- `users` 列表 + getFrequentlyRepliedUsers 列表族 23（依赖 M2 note 生态排序）
- **`users/show` deepStrictEqual 23**（缺 unreadAnnouncements/verifiedLinks/
  followingCount 等字段出处——非列表端点问题，pack 细节缺口）
- `hashtags/users` 15（7 已转绿；余 8 依赖 sort 链路细节/一致性 dict）
- i/update each 残余 10（isBot:true 等字段不在 22 字段子集——M2）
- admin/roles + suspend/delete 2+（BLOCKED）

## 下批（M2 note 生态后自然解锁）
1. `users` 列表（users.ts 端点 users.ts；sortBy/limit/state）
2. `users/search-by-username-and-host` + `users/search`
3. `hashtags/users`
4. 关系族（i/follow/users/followers→users.ts relations 候补）
