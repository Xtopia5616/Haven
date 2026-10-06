# ADR 0543：UI ToolRun 分类跟随生成契约

## 状态

已采纳并实施；UI 类型检查、测试与构建通过。

## 背景

`ui/src/lib/toolRunTerminology.ts` 使用本地 `TaskKind = 'foreground' | 'background' | 'scheduled'` 和 `TASK_KIND_LABELS`。其中 `foreground` 被显示为“会话”，但 session 是对话实体，且 `ToolRun` generated IPC contract 只包含 `background/scheduled`。`foreground` 属于 `ToolExecutionMode` 的执行方式，不能作为 ToolRun 分类。生产调用均来自 `ToolRunKind` DTO 或对应的 `background/scheduled` 字面量；前端 parser 也会拒绝其他 wire kind。

## 决定

1. 删除 UI 私有 `TaskKind`，将标签表命名为 `TOOL_RUN_KIND_LABELS` 并按 generated `ToolRunKind` 键入。
2. `toolRunKindLabel` 接受 `ToolRunKind | undefined`，标签只映射后台与定时 ToolRun；缺失 kind 使用通用“任务”文案。
3. 在命名规范中明确 `ToolExecutionMode::{Foreground, Background}`、持久 `ToolRunKind::{Background, Scheduled}` 和 session 三个概念的边界。

## 替代方案

- 保留 `foreground -> 会话`：拒绝。该映射让 ToolRun 标签 API 接受一个 wire/runtime 不存在的类型值，并把对话实体塞入任务分类。
- 扩展 generated `ToolRunKind`：拒绝。foreground 调用不创建持久 ToolRun，扩展会错误改变 Rust、IPC 和 UI 契约。
- 合并 `ToolExecutionMode` 与 `ToolRunKind`：拒绝。前者选择一次工具调用的执行方式，后者区分可脱离当前 turn 持久运行的后台任务和定时任务。

## 影响与验证

- 仅修改 UI 内部类型/标签映射与审计文档；Tauri/JSON、数据库字段、ToolRun 生命周期和可见后台/定时标签不变。
- 验证通过：`corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`、`scripts/check-adr-index.ps1` 与 `git diff --check`。

## 回滚

恢复原 `TaskKind` 标签映射及对应测试和文档说明。无需 IPC、配置、数据库或用户数据迁移。
