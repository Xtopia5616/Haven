# 1004：ToolRun command rows 必须包含状态

## 状态

已采纳并实施。

## 背景

`ToolRunEvent` 同时用于 Tauri lifecycle event 与 board/history command response。生命周期投影的共享 mapper 允许稀疏字段，而 `list_tool_runs` 和 `list_tool_run_history` 实际由完整 `ToolRunView` 生成，始终包含 status。UI command adapter 复用宽松 mapper，会接受缺少 status 的不完整 command row。

## 决定

- 保留 `mapToolRunPayload` 对稀疏 payload 的既有语义，供 lifecycle event 与通用投影使用。
- 新增 `ToolRunCommandRow`，其 `status` 为必需字段；`mapToolRunCommandRow` 先复用共享字段和 identity 校验，再拒绝缺少 status 的 row。
- board 与 history command adapter 只返回经过 command-row mapper 校验的结果；board 的畸形行继续映射为 `null` 交给 store 记录警告，history 继续丢弃无效行。
- Rust wire DTO、生命周期事件 shape 和任务状态行为保持不变。

## 影响

command rows 进入 UI store 前必定有状态，避免把 command 缺项误当成可增量合并的事件投影；事件仍可使用稀疏 payload projection。此改动不涉及数据库 schema 或 IPC 生成 shape。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
- 对本次改动的 UI 文件运行 Prettier 检查。
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`

## 回滚

无需数据迁移。若撤回 command-row parser，应保留与生命周期 payload 不同的必需 status 校验，避免再次接受不完整 board/history command rows。
