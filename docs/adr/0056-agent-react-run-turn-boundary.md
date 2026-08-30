# ADR 0056：Agent ReAct Run/Turn 边界

## 背景

`react/loop.rs` 同时承担 run 生命周期、步数预算、上下文注入、模型调用、响应
重试、工具批处理和 turn-end 持久化。这样一次模型采样的控制流被大量暂停、确认、
取消和错误分支打断，工具完成顺序也会直接决定 canonical transcript 的写入顺序。

Codex 的 turn 模型将“一次 turn”定义为一次模型采样及其工具调用，工具结果完整回写
后才进入下一次采样；Pi Coding Agent 则将工具/steering 作为内层循环，把 follow-up
留给外层循环。参考实现：

- [OpenAI Codex session turn](https://github.com/openai/codex/blob/main/codex-rs/core/src/session/turn.rs)
- [Pi Coding Agent loop](https://github.com/earendil-works/pi/blob/main/packages/agent/src/agent-loop.ts)

## 决定

- 将 ReAct 控制流拆成三个明确边界：
  - `react/loop.rs` 是 **Run** 驱动器，只负责预算、取消、生命周期检查和 turn
    之间的转移；
  - `react/turn.rs` 是 **Turn**，负责一次上下文注入、一次模型采样及响应策略重试，
    决定是结束、暂停还是执行工具批次；
  - `react/tool_batch.rs` 是 **ToolBatch**，负责确认门禁、并发执行、错误/ask
    聚合以及工具结果物化。
- Turn 的正常顺序固定为：注入上下文 → before-step hook → sanitize → 模型采样 →
  after-LLM 策略 → 搜索/思考投影 → 工具批次或 turn-end。暂停和取消通过显式的
  `TurnOutcome` / `ToolBatchOutcome` / `LoopExit` 返回，不再通过循环内部的隐式
  `continue`/`return` 传播状态。
- 工具可以并发执行，但 `CompletedTool` 先按 assistant 返回的 tool-call 顺序
  缓存，待批次完成后再经 `apply_transcript` 依序物化。这样执行吞吐与 canonical
  transcript 顺序解耦，恢复和 provider 输入不受竞态影响。
- 上下文队列在单次 turn 边界只选择一种用户输入来源：steering 优先；没有 steering
  时才取 follow-up；后台 action 结果始终随批次取出。下一次 turn 再继续取剩余队列。
- 不增加旧 loop 的兼容层，不改变 `ReActSnapshot.events` 的 authority、X12 投影
  规则或快照结构；这是内部控制流重组，旧的模块内调用边界直接删除。

## 替代方案

- 继续扩展单体 `loop.rs`：改动表面小，但每个新暂停/重试分支都会再次扩大状态
  交叉，拒绝。
- 让每个 provider 自己实现 ReAct 循环：会复制工具确认、取消、投影和恢复语义，
  破坏 Agent 的 provider-neutral 边界，拒绝。
- 为新的 loop 引入第二套 snapshot：会制造两个恢复真源；现有 events 已足以表达
  turn 边界，拒绝。

## 影响

这是 Agent 内部控制流重组。`loop.rs` 从约 800 行缩为 run 驱动器，单次采样逻辑
集中在 `turn.rs`；工具完成的 canonical 写入从完成先后改为 assistant 调用顺序，
steering 在下一次模型调用前优先于 follow-up。数据库 schema、快照字段、Agent
事件通道和 dispatcher 外部调用不变。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

重点回归预算恢复、队列优先级、工具确认/ask/取消、批次错误聚合、turn-end、快照和
rollback 测试。

## 回滚

回退本 ADR 对应提交即可恢复旧控制流；不需要数据库或用户数据迁移。若只回退部分
代码，必须同时恢复 `react/loop.rs` 与 `react/turn.rs` 的模块声明，以及
`drain_react_context` 的调用方。
