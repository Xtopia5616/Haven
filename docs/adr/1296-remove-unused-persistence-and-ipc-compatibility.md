# ADR 1296：移除无消费者的持久格式与 IPC 兼容入口

状态：已接受并实施。

## 背景

2026-10-17 兼容层审计核对了当前 writer、reader、renderer 与调用方。部分 alias、缺字段默认、单版本 marker 和 Tauri 命令只为已不存在的格式或消费者保留。Messaging inbox 另有 1 MiB legacy 行上限，高于当前 envelope 契约；ToolResult envelope 的 `retry_safety` 实际承载 `OperationIdempotency`，名称与其语义不符。

## 决定

1. 对只读当前格式且没有 migration/decoder 的 durable DTO 删除旧 alias、旧形状 fallback 和固定单版本标记；缺省仍有语义的可选字段保持可省略，SQL role 列保持不变。
2. Messaging 的 envelope、registry 与 backlog state 要求当前字段形状；未知字段拒绝。JSONL 单行上限统一到当前 serialized envelope contract，损坏或超限 source 保持原样，不在线导入、跳过或迁移旧内容。
3. Messaging 格式切换前必须完整备份并重置整个 `inbox/`，清除 mailbox、processing、backlog/cursor、registry、archive 与 tombstone；SQLite transcript、session 和 memory 不受此 inbox reset 影响。
4. ToolResult envelope 的可信 `OperationIdempotency` 字段在 Rust、IPC 和 UI 中统一命名为 `idempotency`；动态 tool output 不得覆盖它。固定 `ToolResultEnvelope::Default`、workflow DSL version 和已删除状态 projection 字段不再构造伪旧形状。
5. 删除无 renderer caller 的 `search_session_history`、`search_session_history_paginated`、`export_session_history`、`get_recording_state`、`list_permissions`、`delete_tool_run` IPC surface。保留 filtered history、录音 lifecycle/bootstrap 投影、authorization owner，以及 Tools 层 `delete_terminal` capability。
6. 删除无协商行为的 IPC version/count 门面和 retired-command allowlist；Rust handler、Tauri registration、生成 types/shapes、reviewed security metadata 与文档必须具有相同命令集合。
7. `RoutedModel` 和 `RouterConfig` 保持 runtime-only 类型；保留 `RouterConfig::default()`，不为 materialized snapshot 提供 Serde wire contract。删除只验证已拒绝旧 schema 必然拒绝的 `memory_episodes` 特例。

## 影响与重置

- 旧 inbox 不迁移，按完整 inbox reset 步骤处理。
- ToolResult 的持久 event shape 与 schema v45 计划中的其余 session event 持久格式变更一起启用；按发布重置规则备份后删除 `haven.db`、`haven.db-wal` 与 `haven.db-shm`，不导入旧 transcript events。
- 不删除 Tools 内部 terminal-row 删除能力；不改变当前 mailbox atomic rewrite、claim/ack cursor、source preservation、capacity limits、授权检查或 workflow DAG 约束。

## 验收

- Rust workspace tests / strict Clippy / fmt，UI check/tests/build，IPC codegen drift 与 `check-ipc-contracts.ps1` 通过。
- 负向测试确认旧 durable shape、unknown messaging parameter 和旧 UI discriminator/renderer aliases 不再被接受。
- 生成 command map、Rust handler registration、security registry、docs 的命令集合相等。
- 未运行真实数据重置；按文档检查 backup/reset 范围。发布验收前在一次性数据根目录验证重置流程。
