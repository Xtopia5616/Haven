# ADR 1157：从用户消息派生独立会话

- 状态：Accepted
- 日期：2026-10-14
- 范围：Agent、Memory、App IPC、聊天 UI、SQLite schema

## 背景

`rollback_session` 会把当前会话恢复到一个较早的有效路径，随后继续覆盖该会话的投影。它不保留可切换的旧分支。比较不同处理方案时，用户需要从一条历史用户消息创建另一个会话，同时保留原会话。

Haven 当前的 `session_events` 是 append-only 恢复权威，`messages` 和 `session_steps` 是物化投影。将完整树加入每条事件会扩大事件、回放和 UI 契约。本次只需要一个独立 child session。

## 决定

- 增加 `fork_session_from_message(session_id, message_id)` 命令。从有效 transcript 中精确定位所选 `UserInject`，裁出包含该消息的前缀，并在新的 `ses-*` 会话中运行。
- 新会话记录 `SessionOrigin::Fork { parent_session_id, source_message_id }`；会话关系菜单可返回父会话或打开直接子会话。
- 对复制的 Haven-owned `msg-*` / `step-*` 身份重新调用 `new_id`；provider tool-call ID 与受管 `asset-*` 身份保持不变。`ToolCall.message_id` 作为可选 durable identity 保存，以保持 assistant 投影身份关联。
- 新会话记录、transcript events、对应的 rollback branch points、messages 与 steps 在同一个 SQLite 写事务中建立。branch point 的 projection cutoff 映射到子会话共享 Memory 时钟。用量、授权、pending input 与交互等待状态不继承。
- 子会话从所选用户消息重新运行；UI 在执行前说明工具或外部操作可能再次触发。父会话保持不变。
- 只允许从最近 compaction root 之后仍存在于有效事件路径的用户消息派生。已压缩且无原始事件路径的旧消息会明确拒绝，不从投影内容猜测或重建 transcript。
- 不引入通用 session tree event 格式，也不改变现有 rollback 语义。

## 考虑过的方案

- **覆盖式 rollback**：保留现有行为，但无法并排比较方案，也会丢失原会话后续投影。
- **完整事件树**：可以表示多代分叉与导航，但扩大 durable event 与 replay 契约，超出当前产品需求。
- **独立 child session**：重用既有 session、origin 和 lineage 能力，隔离两个时间线，符合本次目标。

## 数据与兼容性

schema v44 为 `sessions` 增加 `fork_source_message_id` 并允许 `origin='fork'`。当前数据库契约不执行运行时迁移；从 v43 升级前按 `docs/release-and-reset.md` 删除 SQLite 数据库文件。配置和 Messaging inbox 无需重置。

新增 IPC 命令由 Rust handler 自动生成 TypeScript 契约。Transcript `ToolCall.message_id` 是可选字段，缺失时仍可读取既有 JSON payload；当前 schema reset 仍是数据库兼容边界。

## 验收与回滚

验收覆盖精确消息身份、被压缩旧消息拒绝、子会话投影身份重映射、rollback boundary 复制、原会话不变、子会话重放以及 IPC 生成/漂移检查。`cargo fmt --all -- --check`、Rust workspace check、严格 Clippy、workspace tests、UI check/全量测试/生产构建及 IPC 契约检查均通过；生成契约检查确认 TypeScript 形状与 Rust handler 一致。

若需撤销功能，移除派生命令和 fork lineage；v44 数据按完整 SQLite reset 处理，不写向后兼容迁移。
