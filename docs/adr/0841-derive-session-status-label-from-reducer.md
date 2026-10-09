# ADR 0841：从 Session reducer 派生会话状态标签

## 状态

Accepted — 2026-10-10

## 背景

Session reducer 已保存 Session 列表与当前选中的 `session_id`。Chat route 从中投影当前会话状态标签，再写进 `activeSessionStatusLabelStore`；应用 shell 订阅这份可变副本展示状态。该标签没有独立生命周期，也不是第二个状态事实，只是 reducer 状态的 presentation 投影。Chat route 与 shell 还各自定义了 Session 标签映射。

## 决定

- 在 `sessionStatus` 集中定义 `sessionStatusLabel(session)` 与 `sessionHistoryStatusLabel(status)`，统一 Session 状态的文案 owner。运行态完成后的标签继续显示“空闲”，历史列表继续显示“已完成”，两者表达当前状态与历史结果的不同语境。
- Chat route 与 shell 均从各自已订阅的 Session reducer projection 派生标签。
- Session history 复用同一模块提供的历史状态映射，删除组件内复制的状态文案表。
- 删除 `activeSessionStatusLabelStore` 及 Chat route 写入该 store 的同步 effect。
- 保留 `reactExecutionPhaseStore`：它表达某个 Session 当前 ReAct turn 的运行阶段，不由 Session status 标签替代。

## 替代方案

- 保留页面写入、shell 订阅的 store：拒绝。它复制 reducer 派生值，引入额外同步边界。
- 将状态标签并入 reducer 持久状态：拒绝。标签完全可由 Session 当前状态计算，不是独立领域状态。

## 影响与验证

改变 UI 内部状态 ownership，不改变 Session reducer、IPC、持久化数据或展示优先级，无需数据重置。验证覆盖标签投影、Svelte 检查与 UI 全量测试；生产构建确认路由和 shell 装配。

## 回滚

若未来 shell 不再能从 Session reducer 订阅状态，可恢复一个明确由 reducer selector 派生的只读 projection；不得恢复页面写入的独立可变标签 store。
