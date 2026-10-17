# users.ts e2e census — misskey-rs（持续更新）

上游规格：misskey-dev/misskey @ ea2c3aed，`packages/backend/test/e2e/users.ts`
harness：`vitest.e2e-rs.config.ts` + `global-setup.mts`（spawn misskey-rs + /env{,-reset}）
运行日志聚类（第 8 轮，run 39064708934，commit 7bc556f~）：

```
Test Files  1 failed (1)
    Tests  104 failed | 75 passed | 1 skipped | 10 todo (190)
```

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
- `users/search` + search-by-username-and-host 族 29
- `users` 列表 + getFrequentlyRepliedUsers 列表族 23（依赖 M2 note 生态排序）
- **`users/show` deepStrictEqual 23**（缺 unreadAnnouncements/verifiedLinks/
  followingCount 等字段出处——非列表端点问题，pack 细节缺口）
- `hashtags/users` 15
- i/update each 残余 10（isBot:true 等字段不在 22 字段子集——M2）
- admin/roles + suspend/delete 2+（BLOCKED）

## 下批（M2 note 生态后自然解锁）
1. `users` 列表（users.ts 端点 users.ts；sortBy/limit/state）
2. `users/search-by-username-and-host` + `users/search`
3. `hashtags/users`
4. 关系族（i/follow/users/followers→users.ts relations 候补）
