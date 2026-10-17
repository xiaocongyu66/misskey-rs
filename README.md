# misskey-rs

Rust 重写的 Misskey 后端。目标是与上游 [misskey-dev/misskey](https://github.com/misskey-dev/misskey)
后端**全功能对标、API 逐字段兼容**：客户端照旧 POST `/api/<endpoint>`（`i` token），
响应形状、错误码、Streaming 协议全部对齐；前端继续使用上游 `packages/frontend` 构建产物。

- 上游 e2e 测试（`packages/backend/test/e2e/`）是兼容性验收规格，在 CI 中直接打本服务器。
- 数据库 schema 与上游逐表逐索引一致（CI 由上游 TypeORM 迁移链生成基线），可与真实实例互换。

## 开发模型

本仓库构建与测试只在 GitHub Actions 上进行（本地不做 cargo 构建）：

```sh
gh repo sync && gh run watch
```

## 运行

```sh
misskey-rs config.yaml   # 默认读取 ./config.yaml，样例见 config/config.example.yaml
```

需要 PostgreSQL（并开启 redis，从 M2 起）。`/healthz` 探活。

## 计划（里程碑）

M0 骨架 → M1 账号认证 → M2 笔记/时间线 → M3 Streaming → M4 Drive →
M5 内容面 → M6 角色管理 → M7 队列外围 → M8 ActivityPub 联邦 → M9 全量兜底。
完整计划见仓库主 discussion / plan 文档。

## License

AGPL-3.0-only（与上游一致）。
