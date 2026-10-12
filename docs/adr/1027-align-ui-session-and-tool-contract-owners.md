# 1027：对齐 UI 会话执行阶段与工具契约命名

## 状态

已采纳并实施（2026-10-12）。

## 背景

UI 的 `sessionRuntimeStore.ts` 只保存 ReAct 执行阶段，并不拥有完整 session runtime。启动恢复流程已按 `resume` 命名，但注释和局部变量仍混用 `restore`。ToolRun `ScheduleMode` 的 renderer presentation 层又重复声明了与契约完全相同的类型和 guard 别名。

## 决定

- 将执行阶段 store 与同名测试文件改为 `reactExecutionPhaseStore`，保持导出和状态行为不变。
- 会话恢复既有流程的说明、局部变量与测试描述统一使用 `resume`；持久 localStorage key `haven.no_auto_restore` 保持原样，避免改变现有用户偏好读取。
- `ScheduleMode` 和 `isScheduleMode` 由 `contracts/toolRun.ts` 直接拥有；工具结果校验器与 schedule renderer 直接消费该契约，删除无语义变化的 presentation alias。

## 影响

只改变 UI 内部模块路径、注释/局部名和类型/guard 导入。Tauri IPC、工具结果 wire shape、localStorage key 与运行时行为不变。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
- `git diff --check`

## 回滚

恢复文件名和导入路径即可；不要重命名 `haven.no_auto_restore`，除非另有明确的持久化迁移决定。
