# ADR 0995：统一持久用量记录身份字段

## 状态

已采纳并实施；Rust workspace、UI、IPC 与文档门禁通过。

## 背景

`LlmUsageRecord` 表示一次持久化模型调用，已有 canonical `usage-*` 身份，但 struct、`usage_recorded` event payload 与 resume IPC 都将该字段输出为泛名 `id`。UI resume parser 只能靠 `usage-*` 前缀推断实体，而 Rust/DB/event/IPC 命名规范要求字段显式指出实体归属。SQLite 物理列 `llm_usage.id` 属于 Repository 存储表示，不是 wire DTO 命名依据。

## 决定

- 将 `LlmUsageRecord.id` 改为 `usage_id`，并同步更新 Memory、Agent、SessionStore 和 UI 消费者。
- 新增的 `usage_recorded` payload 与生成的 `SessionResumeResponse.llm_usage[]` 只序列化 `usage_id`；UI parser 只接受 `usage_id`，拒绝旧泛名 `id`。
- Memory `LlmUsageRecord` 的 Serde reader 保留仅反序列化的 `id` alias，以读取已有 durable event payload；Serde serializer 从不输出 alias。
- SQLite `llm_usage.id`、表 schema、事件类型、`usage_discarded.usage_id`、累计与 rollback 行为保持不变；不需要 schema 迁移或数据重置。

## 替代方案

- 保留 `id` 并只在 resume response 做额外映射：拒绝。这样 durable record 与 IPC 仍有不同身份命名，产生第二个 DTO owner。
- 删除旧 event payload reader alias 并要求重置数据库：拒绝。历史事件只需要字段别名即可读取，schema 与存储列均未变；强制重置会删除无关的会话、记忆与用量。

## 影响与验证

Rust 类型、durable event 新写 payload 与生成 TypeScript resume contract 的字段名变为 `usage_id`；旧 `usage_recorded` payload 中的 `id` 只在 Memory 内部反序列化时兼容。SQLite 物理列和所有 token/cost 语义不变。

验证通过：`cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check`（0 errors / 0 warnings）、`corepack pnpm run test:run`（137 files / 1094 tests）、`corepack pnpm run build`、`scripts/check-ipc-contracts.ps1`（79 handlers）、`scripts/check-ipc-events.ps1`（36 channels）、`scripts/check-adr-index.ps1`（978 uniquely numbered records）与 `git diff --check`。Rust 手动性能 profile 与需真实桌面的 UI clipboard 测试按各自约定 ignored。

## 回滚

恢复 `LlmUsageRecord.id` 和对应的 UI parser、fixture 与 generated contract，然后重新运行 IPC generator。SQLite 数据不变；保留的 reader alias 可在回滚后移除。
