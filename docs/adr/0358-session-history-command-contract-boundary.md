# ADR 0358：Session history command contract boundary

- 状态：已采纳（2026-09-26）
- 范围：活跃会话列表、历史搜索、resume 读取与 MemoryView 会话历史操作的 Tauri command boundary
- 关联：ADR 0350（Session UI field mapping audit）、ADR 0357（Memory command contract boundary）

## 背景与审计

`get_sessions`、`search_history_filtered`、`get_session_for_resume` 与 `get_last_conversation` 的实际 UI 调用仍直接使用通用 `invoke`，返回值因而落到 `any`。MemoryView 自行定义带 `[key: string]: any` 的 `MemorySession`，筛选参数也用 `Record<string, any>`；`resumeMessages.ts` 另有一组与 Rust `SessionResumeResponse` 重叠的 message、step、usage 子结构。MemoryView 中的恢复、重开、删除、清空和改名命令也绕过共享边界。

审计确认 `get_history`、`count_history`、`search_history_paginated`、`count_history_search`、`search_history` 与 `export_history` 当前无 renderer 调用者；它们留在命令 registry，不需要无调用 helper。活跃 session wire 数据目前没有第二个 camelCase mapper：列表页只在 reducer 投影时补 `waitingReason`，历史行和 resume 投影继续读取 snake_case 字段。恢复 interaction normalizer 仍有独立的未知输入、兼容字段与畸形行处理职责，保持 ADR 0350 的结论。

## 决定

1. 在 `contracts/commands.ts` 为分页、搜索、过滤、导出、session id 和标题更新定义命名请求类型；原有 flat Tauri 参数、snake_case Rust 字段到 camelCase invoke 参数边界及 Option 的省略/null 行为不变。
2. 新增 `contracts/sessionHistory.ts`，以 Rust `Session` history row、`SessionInfo` 与 `SessionResumeResponse` wire shape 作为 renderer 消费类型来源。`resumeMessages.ts` 的容错输入从这些类型派生；session usage 与 LLM usage 不再维护一份平行接口。
3. 新增 `sessionHistoryCommands.ts`，为 `get_sessions`、`search_history_filtered`、`get_session_for_resume`、`get_last_conversation`、`reopen_session`、`delete_session`、`clear_history` 与 `update_session_title` 提供 typed direct-forward helpers。`MemoryView`、`+page.svelte` 与 `chatController` 改经这些 helper。
4. 不新增运行时校验、字段转换、筛选、排序或错误包装。命令 response 和 rejection 原样返回；`SessionHistoryRow` 的 TypeScript shape 不会在运行时删除新增字段。
5. IPC registry 将 history list response 名称从泛化 `Session[]` 明确为 `SessionHistoryRow[]`。Rust command handlers、命令名、输入 wire payload、响应 JSON 与数据库均不变。扩展 IPC 检查比较 Rust/TypeScript request/response registry、历史行字段和 request fields，并拒绝 UI 绕过 helper。

## 兼容性与影响

历史页仍以 50 条分页、相同 offset、status/date/query 参数调用 `search_history_filtered`；刷新顺序和 stale response guard 不变。resume、错误会话只读判断、reopen 时机、通知和 catch/rejection 行为不变。列表的单一 `waiting_reason`→`waitingReason` reducer 投影保持原样，恢复 transcript 与 interaction normalizer 不变。

无 Rust handler 或 DTO 改动、无 snake_case wire payload 改动、无 DB/ID/X12 变化、无全局 codegen。未调用的 legacy history commands 没有新增 UI helper。

验收命令：

```sh
corepack pnpm run check
corepack pnpm run test:run
corepack pnpm run build
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
pwsh -NoProfile -File scripts/check-ipc-events.ps1
cargo fmt --all -- --check
cargo test --workspace --locked
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
git diff --check
```

## 回滚

回滚本提交即可恢复原直接 invoke 与局部历史类型，并移除 command helper、contract DTO、IPC 检查、registry 文案和本 ADR/路线图记录。没有数据、schema 或配置重置要求。
