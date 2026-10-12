# 1043：统一回滚投影时间水位

## 状态

已采纳并实施（2026-10-12）。

## 背景

`BranchPoint.event_cursor` 截断 append-only `session_events`；`last_msg_at` 是历史字段名，用于按时间截断 `messages`、`session_steps` 与 `llm_usage`。此前 cursor 只读取最后一条 message，而 step 和 usage 可以在它之后写入。rollback 随后按 `created_at > last_msg_at` 删除投影，可能误删 branch point 之前仍有效的执行状态和用量。

此外，各 projection writer 使用的 RFC3339 精度并不一致；连接池允许多个 SQLite connection 并行使用，单独读取水位再插入也不能保证并发写入严格有序。

## 决定

- 将 `last_msg_at` 明确定义为三个 durable projection 表的共享 cutoff；branch cursor 从 `messages`、`session_steps`、`llm_usage` 读取最大时间戳。错误 partial 的 `partial_messages` scratch 表不参与水位。
- Memory 统一将时间戳规范化为 UTC 毫秒格式。读取已有行时用 SQLite `strftime` 规范到其毫秒桶；不合法时间戳使读取失败关闭。
- `messages`、`session_steps` 与 `llm_usage` 的写入必须在 `BEGIN IMMEDIATE` 事务内取得全局水位并插入。新值严格晚于该 session 现存的所有 projection 与当前 batch 中先前生成的值。usage durable event payload 与物化行共享同一时间戳。
- 将 `SCHEMA_VERSION` 提升为 40。既有 branch point 只存 message-only cutoff，且 append-only event 不允许安全回写；无法从其 payload 无歧义重建旧分支完整投影边界。升级需重置 SQLite 数据库，避免继续使用含旧 branch point 的 durable timeline。

## 影响

不改变表结构、IPC 或配置。升级前需完全退出 Haven，并删除 `haven.db`、`haven.db-wal` 与 `haven.db-shm`；该重置会清除会话、记忆、ToolRun 与用量。配置和独立 inbox 无需重置。重置步骤见 [`release-and-reset.md`](../release-and-reset.md)。

## 验证

- 回归测试验证 branch cutoff 覆盖 message、step、usage，包含旧高精度 RFC3339 时间戳，并验证截断仅移除 branch 之后的 projection。
- `cargo fmt --all`
- `cargo test --locked -p haven-memory branch_projection_watermark_covers_messages_steps_and_usage`
- workspace locked check、strict Clippy 与 tests。

## 回滚

若需恢复旧实现，必须回到与 schema v39 匹配的二进制和数据库备份。schema v40 数据库不得由旧二进制打开；无匹配备份时按项目数据重置流程创建新库。
