# ADR 1297：统一 SessionEvent 与持久时间戳契约

状态：已接受并实施。

## 背景

SessionEvent writer 始终写 `event_version = 1`，reader 只比较固定值，没有版本分发或 decoder。共享 projection clock 已生成固定毫秒 UTC RFC3339，但 watermark reader 仍让 SQLite `strftime` 归一化旧 offset 和超精度值。ToolRun writers 生成 `created_at` 的 SQLite datetime 字符串，reader 再兼容该格式；ToolRun 其它时间也未统一固定毫秒 UTC 表示。

## 决定

1. 移除 SessionEvent 的 `event_version` 常量、DTO 字段、数据库列、读写与 Agent plumbing。将来如需不同事件格式，必须实现明确版本化 decoder。
2. Session messages、steps 与 usage 的 projection watermark 按原始值逐行验证固定毫秒 UTC RFC3339，再按字典序取最大值；offset、无毫秒及超精度时间均 fail closed。保留共享 Memory projection clock、同事务写入、严格递增和 rollback cutoff 双时钟语义。
3. ToolRun `due_at`、`started_at`、`finished_at` 与 `created_at` 统一保存为 `YYYY-MM-DDTHH:MM:SS.sssZ`。输入边界可将有效 RFC3339 归一到该格式；持久读取只接受精确 canonical 值。SQLite schema 对这些列施加当前格式约束，`created_at` 由 Rust writer 显式提供，不使用 `datetime('now')` 默认值。
4. ToolRun 对 `due_at` 的 quarantine 保留：正常写入拒绝无效时间；测试以 `PRAGMA ignore_check_constraints` 显式模拟已有损坏行，验证 quarantine 仍可将其移入终态记录。
5. v45 是同一未发布的破坏性契约，包含 Slice 3 durable ToolResult shape 与本决定的 SessionEvent/时间格式变化；不再单独 bump schema version，也不读取旧格式或导入旧事件。

## 影响与重置

- 旧 schema 不迁移。切换前备份后删除 `haven.db`、`haven.db-wal` 与 `haven.db-shm`，重启创建空 v45 数据库；会清除会话、transcript、记忆、权限、ToolRun、用量和事件。
- 不执行当前用户数据重置。`update_message_created_at` 保留为测试/test-support 写入点，没有生产调用方。
- Rollback 仍以 `event_cursor` 截断 append-only event，以共享 `last_msg_at` 投影 cutoff 清理 messages、steps 与 usage；ToolRun completion outbox 身份与事务边界不变。

## 验收

- 测试覆盖 event append/replay、projection watermark 三表合并、非 canonical 时间拒绝、ToolRun schema 时间约束、ToolRun timestamp normalize/strict read、outbox claim/ack/reconcile、rollback 和 usage compensation。
- Rust workspace locked tests 与严格 Clippy、格式检查通过。
- 真实用户 DB reset 未执行；发布前需按 reset guide 在一次性数据根目录验证备份与清理步骤。
