# ADR 0057：Agent ReAct Run 状态机与单一运行态

## 背景

Run、Turn、ToolBatch 已经按控制流职责拆分，但它们仍通过三组独立的可变参数传递
`events`、`canonical` 和 `branch_points`。这使得每次新增暂停、恢复、压缩或重试分支
都要重复维护一组借用关系，也容易让一个边界只更新其中一部分状态。

Codex 的 Session/Task/Turn 模型把一次任务的生命周期状态放在显式的 turn 驱动器中；
Pi Coding Agent 则把模型请求、工具批次、steering 和 follow-up 组织成可观察的外层/内层
循环，并在模型请求边界转换消息。参考实现：

- [OpenAI Codex protocol v1](https://github.com/openai/codex/blob/main/codex-rs/docs/protocol_v1.md)
- [OpenAI Codex session turn](https://github.com/openai/codex/blob/main/codex-rs/core/src/session/turn.rs)
- [Pi Coding Agent loop](https://github.com/earendil-works/pi/blob/main/packages/agent/src/agent-loop.ts)

## 决定

- 引入 `react::ReActState` 作为一次运行的唯一内存状态，统一持有：
  - `events`：可恢复的 transcript 权威；
  - `canonical`：由 transcript 物化出的当前模型上下文；
  - `branch_points`：相对于当前 event log 的回滚索引。
- Run、Turn、ToolBatch、turn-end、快照和恢复边界只接收这个状态对象，不再平行传递
  三个可变集合。这样状态的整体性由类型边界保证，生命周期仍由 Run 驱动器控制。
- `apply_transcript` 继续是唯一的 durable transcript writer。CompactSummary 替换
  `events` 与 `canonical` 时同时清空 branch points，避免保留指向旧日志的游标。
- provider 请求使用 `canonical.clone()` 形成请求态，在请求边界执行 sanitize；sanitize
  修复不会反写 canonical，也不会在恢复后悄悄消失。
- 工具失败后的 retry nudge 使用 `RetryNudge` 作为一次性控制态，只附加到下一次 provider
  请求的失败 observation；它不进入 events、canonical、snapshot 或 messages。
- 暂停/确认/ask/取消只 checkpoint 当前 `ReActState` 并改变 session lifecycle；已经由
  transcript writer 投影的 assistant 内容不再由 pause 路径二次写入。
- 这是内部架构重构，不增加兼容层，不改变数据库 schema、snapshot wire shape、IPC
  事件或外部 AgentLayer 调用契约。

## 替代方案

- 继续把三个集合作为 positional arguments 传递：改动小，但状态一致性依赖每个调用点的
  纪律，拒绝。
- 让各模块持有自己的 canonical/events 副本：会重新引入双真源和隐式合并，拒绝。
- 把 retry nudge 写入 canonical：暂停或恢复时会把内部控制语句伪装成 transcript 内容，
  拒绝。
- 为新状态引入第二套 snapshot：没有必要，并会破坏 `ReActSnapshot.events` 的单一
  恢复权威，拒绝。

## 影响

Run/Turn/ToolBatch 的函数契约更短，状态变更路径更容易审计；并发工具仍按 assistant
tool-call 顺序物化，steering/follow-up 顺序和原有生命周期语义保持不变。provider 请求的
sanitize 与失败重试提示成为明确的 ephemeral request layer，暂停和恢复不会持久化它们。

不兼容项仅限 crate 内部私有调用边界；数据库、snapshot、Tauri 和 UI 不需要迁移。

## 验证

```text
cargo fmt --all -- --check
cargo check --locked -p haven-agent
cargo test --locked -p haven-agent --lib
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

重点回归共享状态迁移、CompactSummary 清除 branch points、工具结果顺序、retry nudge
不污染 canonical、确认/ask/取消、快照恢复和 rollback。

## 回滚

回退本 ADR 对应提交即可恢复旧的参数传递方式；不需要数据库、snapshot 或用户数据
迁移。若只回退部分代码，必须同时恢复 `ReActState` 的创建点、Run/Turn/ToolBatch 的
调用契约以及 `apply_transcript` 的测试夹具。
