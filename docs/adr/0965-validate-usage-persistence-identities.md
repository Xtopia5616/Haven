# ADR 0965：校验用量持久化补偿身份

## 状态

已接受并实现（2026-10-11）。

## 背景

`SessionStore::append_usage_batch_cancellable` 将用量记录与 `usage_recorded` 事件写入 SQLite；新用量 ID 由 Memory 使用 Common generator 创建，但传入的 session ID 没有在写入边界验证。`discard_usage_cancellable` 将调用方给出的 usage ID 写进 append-only `usage_discarded` 事件，再删除相应投影行；非规范 ID 也能生成补偿事件。

Agent 当前从已写入的用量记录取得规范 ID，但 SessionStore 是持久化 owner，应在写入事件和投影前校验其实体身份。

## 决定

- 非空 usage batch 写入前，要求 `session_id` 是 canonical `ses-*`。
- usage discard 追加事件前，要求 `session_id` 是 canonical `ses-*`，并要求 `usage_id` 是 canonical `usage-*`。
- 两项校验均复用 `haven_common::types::is_canonical_id`；拒绝输入时不打开事务、不写事件、不更新或删除用量投影。

## 替代方案

- 只依赖 Agent producer mint ID：无法约束 SessionStore 的公开写入口，也不能保护未来调用方。
- 在事件写入后再检查：会让非法身份进入 append-only 权威事件流，无法通过简单回滚清除已提交事件。

## 影响

仅收紧 Memory usage 写入边界。合法用量与 rollback compensation 行为不变；usage IDs 仍由 Memory 生成。无 schema、event payload shape、IPC 或配置变化，无需数据重置。

## 验证

- Repository regression test 覆盖 malformed session 在用量写入前被拒绝，malformed session/usage ID 在 discard 前被拒绝，拒绝时事件与投影保持不变。
- 既有 projection truncate 测试继续验证合法 usage discard 事件可 replay。
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

移除写入口的 canonical ID 检查即可恢复旧行为。没有持久化或配置迁移。
