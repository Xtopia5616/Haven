# ADR 0357：Memory command contract boundary

- 状态：已采纳（2026-09-26）
- 实施更新：[ADR 0529](0529-single-session-lifecycle-event-and-memory-response-dto.md) 将 repository `Fact` 与 IPC wire DTO 分开；原前端调用、命名请求 DTO 和 `memoryCommands.ts` owner 继续有效。
- 范围：MemoryView 的 `list_facts`、`add_fact`、`delete_fact` 与 `recall_memory` Tauri 调用
- 关联：ADR 0285（Fact command 的 MemoryFactStore boundary）、ADR 0356（Phase 8 状态校准）

## 背景与审计

Rust `Fact` 与 app-binary `MemoryRecallItem` 已分别定义稳定响应字段，四个命令也已在 Rust/TypeScript IPC registry 登记 request 与 response 名称。实际前端调用仍由 `MemoryView.svelte` 直接使用返回 `any` 的通用 `invoke`；facts store 使用 `any[]`，recall 使用 `Record<string, any>[]`，`MemoryRecall.svelte` 又定义了一个字段不完整的局部结构。请求 shape 没有可供调用点共用的命名 TypeScript interface。

审计未发现重复的 camelCase mapper：当前事实组件读取 snake_case 字段，recall 结果也通过 `entity_id` 关联。MemoryView 中的 `search_history_filtered`、会话恢复/删除/重命名等仍是另一条 session-history command family，且 `MemorySession` 仍有宽松的历史投影类型；本切片不收口它们。`run_memory_maintenance` 属于 Settings 命令，也不在本切片内。

## 决定

1. 在 `contracts/commands.ts` 定义 `RecallMemoryRequest`、`ListFactsRequest`、`AddFactRequest` 与 `DeleteFactRequest`，字段遵循 Tauri 扁平 payload 和现有 camelCase 参数名；可选参数继续允许省略或显式 `null`。
2. 在 `contracts/memory.ts` 定义 UI 类型。当前 `Fact` / `FactSourceRef` 是 Rust `MemoryFactResponse` / `MemoryFactSourceRef` 的生成类型别名；ADR 0529 取代 repository `Fact` 直接作为 IPC wire type 的实现方式。`MemoryRecallItem` 与 UI 补充 `kind` 后的 `MemoryRecallResult` 保持原有契约。
3. 新增 `memoryCommands.ts`，四个 helper 以命名 request/result 类型调用原有 Tauri command，并原样返回 response/rejection。`MemoryView` 仅通过 helper 调用事实 CRUD 与 recall。
4. 保留当前 snake_case response 字段，因为现有记忆视图直接消费这些字段。该边界不增加 mapper、runtime validation 或错误包装；未来需要 route-facing camelCase DTO 时，应在单独切片明确迁移所有消费者及兼容语义。
5. 用 helper 单测固定扁平请求和 response identity（包括未知附加字段）；IPC contract script 对比 Rust command 参数、Rust response DTO 与 TypeScript contracts，并固定四个 helper 的直接 invoke 转发与 MemoryView 调用边界，因此没有错误包装路径。

## 兼容性与影响

Rust command names、参数名、Option 默认行为、limit/kind 选择、wire 响应、事实排序、recall 并行执行、未知响应字段、invoke 日志/错误传播和 UI catch/通知路径均不变。无 Rust、DB、ID、事件、X12 或全局 codegen 变化。

验收命令：

```sh
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
pwsh -NoProfile -File scripts/check-ipc-events.ps1
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
git diff --check
```

## 回滚

回滚本提交可恢复 `MemoryView` 的直接 invoke 和旧局部类型，并删除 memory command helper、contract types、测试、IPC boundary assertions、本 ADR 与路线图进展记录。不需要 Rust 修改、IPC 迁移或数据重置。
