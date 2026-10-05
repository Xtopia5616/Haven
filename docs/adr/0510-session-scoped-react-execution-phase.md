# ADR 0510：ReAct phase 保留来源 session 身份

## 状态

已采纳；实现与 UI 门禁通过（2026-10-05）。

## 背景

`reactExecutionPhaseStore` 原先只保存一个全局 phase。Shell 用它展示最近的 ReAct 活动；聊天页也用同一个值决定当前 Composer 是否处于中断模式，并在提交时判断当前消息是否为 turn 内 steering。后两个消费者声称该 phase 只适用于 active session，但 store 没有 source identity，无法执行这个约束。

`agent:action`、`agent:observation` 和生命周期事件可以由非选中 session 触发，并直接覆盖这个标量。`streamAggregator` 与 `agent:stream_stalled` 已对 active session 过滤，而其他 phase writer 没有相同身份检查。新增回归复现：active session transcript 为空时，后台 session 的 `generating` 会把第一条新消息错误标为 `steering`；同一状态也会使 Composer 把发送按钮切为中断。

## 决定

1. phase snapshot 保存 `{ sessionId, phase }`，所有生命周期、Action/Observation、stream 与 stalled writer 都携带事件自身的 session ID。phase 仍由同一个 runtime store 持有，不增加第二份全局/active 真值，也不改变事件 payload。
2. Shell 的 `WorkspaceStatus` 延续最近 ReAct 活动的显示语义；聊天页的 Composer 和 `submitTranscript` 只在 snapshot 的 `sessionId` 等于当前 active session 时读取 phase，否则视为 `idle`。Session busy 状态仍由原 session 状态列表判断。
3. 单 session 删除只在当前 snapshot 属于该 session 时清 phase；清空所有 session 清除全局 snapshot。终态事件继续将其来源 session 的 phase 置为 `idle`。

## 替代方案

- 在每个 writer 丢弃非 active session 的事件：拒绝。Shell 的最近 ReAct 活动状态和多个 session 的 busy summary 仍需要接收合法后台生命周期事件，且会分散重复的 active-session 判断。
- 另建 active phase store：拒绝。同一事件会同时更新全局与 active 两份 phase，形成双写及同步顺序问题。
- 改成 per-session phase map：暂不需要。Composer 当前只需判断其 active session 的 phase；若后续产品需要并行显示每个 session 的精确 ReAct phase，再按该消费者需求另行评估。

## 影响与验证

- 后台 session 的 phase 不再令当前空闲 session 的首条输入错误标为 steering，也不再使其 Composer 进入中断模式。
- Shell 状态、session busy fallback、event wire、IPC 和持久化不变；无需数据重置。
- 回归覆盖 phase 来源身份、只向来源 session 投影 phase、空 transcript 下跨 session 不窃取 steering，以及原有 streaming/tool phase 和 submit 行为。
- 验证通过：`corepack pnpm run check`（0 error、0 warning）、`corepack pnpm run test:run`（122 files、983 tests）。ADR 索引检查覆盖 493 条记录且本地链接通过。

## 回滚

可还原 string-only `reactExecutionPhaseStore` 并撤销 Composer/submit 的 session 身份过滤；不影响数据、事件或外部契约。若回滚，同步标记本 ADR 为撤销，并记录跨 session phase 仍可能影响 Composer 的边界。
