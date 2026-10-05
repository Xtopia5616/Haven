# ADR 0480：Session-scoped KV 孤儿清理单一 owner

## 状态

已采纳并实施（2026-10-05）。

## 背景

`kv_store` 拥有 session-scoped extraction state 和 Memory event cursor 的 key 语义，也提供 Memory maintenance 使用的 orphan 清理。`sessions::delete_old_sessions` 为在 retention purge 后立即清理同一批孤儿 marker，复制了完全相同的 DELETE predicate 和 key→session ID `CASE` 解析表达式。历史上 event cursor、episode-pending 与 episode-done marker 各自演进时，两处 SQL 都需要同步更新。这是重复业务规则的两个 owner，存在未来新增 key 时遗漏一处而延迟清理孤儿状态的风险；目前没有发现两处行为已经漂移。

## 决定

1. 将 orphan session-scoped KV DELETE 和 key owner 解析保留为 `kv_store` module 的唯一 connection-level helper。
2. `Database::cleanup_orphan_extraction_cursors` 与 `sessions::delete_old_sessions` 都调用该 helper；retention 调用方将原先已持有的 `&Connection` 传入，不另行 checkout connection。
3. 保持现有 SQL、调用顺序、retention 返回值、cache invalidation、自动提交和连接锁范围不变。此 helper 只负责已无对应 `sessions.id` 的 extraction/event-cursor markers；定向单 session 删除和清空所有 session 的路径仍保留其原有优化与事务边界。
4. 不改变 `kv_store` key 格式、schema、维护调度、外部 API 或数据重置契约。

## 替代方案

- 保留两份 SQL 并依靠评审同步：拒绝。历史已证明每次 namespace 增加都会重复修改两处。
- retention 调用公开 cleanup 方法：拒绝。该方法会重新 checkout connection，可能改变连接池资源占用与当前删除/清理之间的并发窗口。
- 将所有 session 删除、清空与孤儿回收统一为同一条全表扫描：拒绝。它会替换当前定向删除和 `clear_sessions` 的事务执行方式，超出本 slice 且无必要。

## 影响与验证

清理 predicate 与 owner 解析只有一个实现。maintenance cleanup 与 retention purge 的既有回归测试继续覆盖：只删除无 owner 的 session markers、保留活 session 的 marker，并清理 extraction、episode 和 event-cursor key。生产调用仍使用原连接，SQL 行为和事务边界不变；无 schema 或持久格式变更，无需数据库 reset。

验证：

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-memory`（389 passed，2 ignored）
- `cargo check --locked -p haven-memory`
- `cargo clippy --locked -p haven-memory -- -D warnings`
- `git diff --check`

## 回滚

恢复 `cleanup_orphan_extraction_cursors` 中的原 SQL，并将相同 SQL 放回 `sessions::delete_old_sessions`；无需数据迁移或重置。
