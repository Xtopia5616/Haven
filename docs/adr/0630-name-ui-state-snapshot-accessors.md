# ADR 0630：命名 UI 状态快照与录音时长 accessor

## 状态

已采纳并实施。

## 背景

`SessionReducer.getState()` 返回当前 reducer state，不接受查询 key；`RecordingOverlayController.getState()` 也返回 controller 当前状态快照。两者都使用 `get` 动词，但不是按 key 查询。录音 controller 的 `getDuration()` 返回 timer 递增的秒数，名称未声明单位。

## 决定

1. `SessionReducer` 与 `RecordingOverlayController` 的同步状态读取统一使用 `snapshot()`。
2. 录音计时读数使用 `durationSeconds()`，明确数值单位。
3. `RecordingOverlayController.state` 与 `.duration` 保留为响应式 Svelte store；同步 accessor 是单次快照/数值读取，不替代订阅接口。

## 替代方案

- 保留 `getState()`，认为它是 JavaScript 常见约定：拒绝，本项目命名规范将无查询键的状态读取统一为名词式 accessor。
- 将 `getDuration()` 改成 `duration()`：拒绝，单位不明确，且 controller 已有 `duration` store 字段。
- 删除同步 accessor 并要求所有调用者订阅 store：拒绝，初始化和 reducer 逻辑需要同步读取当前状态。

## 影响与验证

- 这是 UI 内部 TypeScript API 重命名，没有状态形状、事件或 IPC 变化。
- 命名审计 §5.7 继续覆盖 UI store/controller accessor 与其余组件、contracts 名称。
- 验证：`pnpm run check`、`pnpm run test:run`、`pnpm run build`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `getState()` / `getDuration()` 方法名并同步其调用点；无需数据或 wire 迁移。
