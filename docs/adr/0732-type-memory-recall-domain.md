# ADR 0732：记忆检索输入复用 Memory domain enum

## 状态

已采纳并实施。

## 背景

`haven_memory::embeddings::MemoryEntityKind` 已在 recall query、embedding lifecycle 中拥有闭合的 `Fact` / `Episode` 领域词汇；它的 `parse` 同时负责文本边界的空白与大小写归一化。Tauri `recall_memory` 却接收 `Option<String>`，再在 handler 内重复解析。生成 IPC 因此把 `kind` 暴露为任意 string，UI `MemoryRecallState.kind` 和附加到结果上的 `kind` 也都是开放字符串。

唯一命令 wrapper `memoryCommands.recallMemory` 由 `MemoryView.runRecall` 调用。`fact` 与 `episode` 各触发一次 `recall_memory`；用户可见的 `all` 仅是 renderer 筛选项，在 UI 内展开为两个并行请求。wrapper 的结果没有后端 kind 字段，MemoryView 按对应请求把闭合 kind 附回每条结果。`MemoryCenter` 和 `MemoryRecall` 组件共用同一个 UI 筛选 state。

省略 `kind` 时 Rust handler 默认 `fact`，省略 `limit` 时默认 5；Memory 的 `MemoryQuery` 继续限制 query 和 limit，retriever 继续过滤敏感事实。检索是只读流程，无配置或数据库写入；Memory/Agent 查询错误以 Tauri `String` 返回，`MemoryView.runRecall` 清空结果、记录错误并保留可重试状态。无效 enum wire 值由 Tauri 反序列化拒绝，规范的 `fact` / `episode` 行为不变。

## 决定

- 给 `MemoryEntityKind` 增加 Serde snake_case string 表示，作为 `fact` / `episode` 的唯一闭合 IPC 输入 vocabulary。
- `recall_memory.kind` 改为 `Option<MemoryEntityKind>`；省略时直接使用 `MemoryEntityKind::Fact`，并保持 query、limit 和检索行为不变。
- IPC generator 从 Rust enum 导出 `MemoryEntityKindInput`；Memory UI 的 request kind、结果 kind 与 state filter 分别收窄为生成 enum，或生成 enum 加 UI-only `all`。
- `MemoryRecall` 与 `MemoryCenter` 在 MaterialSelect string callback 边界只接受当前三个筛选值；`all` 仍由 `MemoryView` 展开，不进入 backend command。
- `MemoryEntityKind::parse` 保留给文本解析调用方；命令入口不再维护一条开放字符串解析路径。

## 替代方案

- 保留 `Option<String>` 并继续在 handler 解析：拒绝。Memory 已是该领域词汇的 owner，handler 与 TS request 都不应再维护开放 string contract。
- 把 `all` 加入 `MemoryEntityKind`：拒绝。后端 memory query 每次只在一个 entity kind 上检索；`all` 是 renderer 的并行 fan-out 选择，不是 Memory domain kind。
- 把 kind 放到 `MemoryRecallItem` response：拒绝。结果由特定请求产生，UI caller 已知请求 kind 并在映射时补上；改变 response 只会重复传递已有上下文。

## 影响与验证

Tauri request 的静态 shape 从 `string` 收窄为 generated `MemoryEntityKindInput`，规范 JSON 字符串保持 `fact` / `episode`。未传 kind 仍默认 `fact`；显式传空白或非规范大小写值不再由 handler 容错解析。没有配置、数据库 schema 或持久数据变化，不需要重置数据。`all` 仍留在 UI，并发 fan-out、每种 kind 的 limit、敏感事实过滤、错误显示与重试行为不变。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI `check` / `test:run`（125 files / 992 tests）/ `build`、`scripts/check-ipc-contracts.ps1`（80 handlers）、`scripts/check-ipc-events.ps1`（35 channels）、`scripts/check-adr-index.ps1`（715 ADRs）与 `git diff --check` 均通过。

## 回滚

如回滚，必须同时恢复 `recall_memory.kind: Option<String>`、handler parse/default 路径、生成 IPC request、Memory UI 类型与相关文档。该回滚不涉及持久数据。
