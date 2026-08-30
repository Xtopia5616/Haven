# ADR 0010：Agent 回合结束边界

## 背景

`haven-agent` 的 ReAct 热点已经把循环、投影、恢复和队列拆成多个文件，
但 `react/inject.rs` 仍同时负责待处理上下文注入与回合结束。两条路径拥有
不同的不变量：注入必须保持队列顺序并通过 `apply_transcript`，回合结束必须
先落最终事件、处理并发到达的上下文，再保存分支点并暂停。继续把它们放在
同一个实现块中，会让新增队列/收件箱逻辑意外改变结束和恢复语义。

## 决定

1. 将回合结束流程迁移到 `crates/agent/src/react/turn_end.rs`。
2. 以 `TurnEndInput` 作为唯一调用入口，集中承载 transcript、canonical、
   branch points、最终文本和 provider thinking 数据；循环只负责分类响应并
   提交该输入对象。
3. 保持 `ReActEngine::apply_transcript` 为事件→投影的唯一 ReAct 写入口；
   `turn_end` 不直接访问队列，只调用注入模块暴露的行为入口。
4. 保留现有“最终内容之后再注入 pending context”的顺序，以及无待处理输入
   时的 `Paused` 结果，不改变数据库字段、快照格式或 IPC 行为。

## 替代方案

- 继续在 `inject.rs` 中增加注释：不能形成可检查的职责边界，拒绝。
- 将回合结束逻辑内联到 `loop.rs`：会扩大循环热点并重新混合持久化与模型
  响应分类，拒绝。
- 此阶段抽出跨 crate 的新 trait：当前边界仅在 Agent 内部，过早引入动态
  trait 会扩大生命周期和错误传播面，暂不采用。

## 影响

- Agent 内部结构新增一个只负责回合结束的模块；`inject.rs` 保留上下文的
  transcript 投影，来源读取由后续的 `ContextSource` 边界负责（ADR 0011）。
- `TurnEndInput` 是后续拆分恢复/投影时可复用的稳定输入边界。
- 这是机械重构，不改变已有数据库、快照、事件或用户数据，因此无需重置。

## 验证

```text
cargo fmt --all -- --check
cargo check -p haven-agent
cargo test -p haven-agent --lib -- --test-threads=1
cargo test -p haven-agent -- --test-threads=1
```

重点回归现有 final-answer、文本结束、暂停后继续、并发 steering 和快照恢复
测试；若任一项失败，回退本 ADR 对应的模块迁移即可。

## 回滚与重置

回滚时恢复 `finish_turn_end` 与 `TurnEndOutcome` 到 `react/inject.rs`，删除
`react/turn_end.rs`，不需要删除数据库、配置或缓存。
