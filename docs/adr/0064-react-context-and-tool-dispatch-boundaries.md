# ADR 0064：ReAct 上下文组装与统一工具调度边界

## 背景

ReAct 的上下文输入曾由多个位置分别读取：本地 steering/follow-up、后台
action result、跨会话 inbox 在 turn、hook 和 turn-end 路径中各自处理。工具
调用也把准入、并发执行、取消修复、确认恢复和有序投影揉在一个批次函数里。
这会造成重复消费 inbox、消息优先级漂移，以及单工具路径和并行路径的身份/生命周期
不一致。

## 决定

- `react/context.rs` 在 turn-start 组装一次完整的 `PendingContextBatch`，按
  steering/answer、follow-up、action result、cross-session 的固定顺序排列；
  `inject.rs` 是唯一投影入口。turn-end 只补收采样期间到达的本地队列，inbox
  claim 只有在 transcript 和 snapshot 成功持久化后才确认。
- 一个 tool call 也必须先形成 `ToolBatchPlan`，通过和多工具批次相同的准入、执行、
  取消修复和 ordered result slots；确认恢复复用原始 `step_id`、`action_index`、
  `tool_call_id`，不重新生成身份。
- 工具执行完成顺序可以并发/乱序，但 canonical observation、消息投影与下一次
  provider 请求只按 plan 顺序提交。失败、拒绝和取消均形成显式 observation，
  不静默丢失 pending action。

## 替代方案

- 让 hook 和 turn-end 各自 poll inbox：实现短，但同一 turn 会有多个消费边界，
  崩溃恢复和 receipt 时序难以证明，拒绝。
- 为单工具保留快捷执行分支：代码少，但会绕过批次身份、并发门禁或取消修复，
  拒绝。
- 按完成顺序直接写 canonical：吞吐略高，但下一轮上下文依赖调度时序，拒绝。

## 影响与验证

这是 Agent crate 内部控制流重构，不修改 snapshot、数据库 schema、工具输入/输出
wire 结构或用户数据。重点验证上下文来源优先级、inbox claim durability、重复
message id、单工具 identity、并行结果排序、取消 observation 和确认恢复。

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

## 回滚

回退本 ADR 对应提交即可恢复原有上下文消费和工具批次实现；不需要数据库或用户
数据迁移。
