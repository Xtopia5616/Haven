# ADR 0011：Agent 待处理上下文来源边界

## 背景

ReAct 运行时需要同时处理 follow-up、ask answer、steering、后台任务结果和
跨会话 inbox。此前这些队列/inbox 读取、格式化和 transcript 投影都发生在
`ReActEngine` 的注入方法中，新增来源容易直接修改事件或 canonical，难以保证
来源顺序和 X12 写入路径。

## 决定

1. 使用 `crates/agent/src/react/context.rs` 的 `ContextSource` 负责读取并聚合
   队列/inbox，返回拥有所有权的 `PendingContextBatch` / `PendingContext`。
2. `ContextSource` 保留既有顺序：follow-up → steering → action result；answer
   只携带 `clears_ask` 信号，由注入边界清除持久化 ask gate。
3. `crates/agent/src/react/inject.rs` 只负责把已收集的上下文转换成
   `TranscriptEvent::UserInject` 并调用 `apply_transcript`；不得从队列读取原始
   数据或绕过事件投影。
4. 跨会话 inbox 的 heartbeat、通知轮询、receipt 和低信任格式化继续由来源
   侧处理；格式、限长和控制字符净化规则保持不变。

## 替代方案

- 让每个调用点分别读取三类队列：会破坏单次 drain 和既有顺序，拒绝。
- 让 `ContextSource` 直接写 events/canonical：来源与投影耦合，无法独立测试，
  且会削弱 X12 的唯一 writer，拒绝。
- 将 inbox 放入 `haven-tools`：tools 只提供总线能力，Agent 才拥有“何时注入”
  的会话语义，职责不匹配，拒绝。

## 影响

- `ReActEngine` 新增一个持有 executor、database 和 messaging sidecar 的来源
  适配器；`inject` 只消费其结果并负责投影。
- 不改变队列、inbox、事件、canonical、快照或数据库格式；不需要用户重置。
- 后续可为来源批次增加容量/背压策略，而不触碰回合结束和 transcript 投影。

## 验证

```text
cargo fmt --all -- --check
cargo check -p haven-agent
cargo clippy -p haven-agent -- -D warnings
cargo test -p haven-agent -- --test-threads=1
```

重点回归既有上下文注入顺序、ask answer 清除、后台 action result 和跨会话
消息净化测试。

## 回滚与重置

回滚时将 `ContextSource` 的读取逻辑恢复到 `react/inject.rs`，删除
`react/context.rs`，并恢复 `ReActEngine` 对 `MessagingPoller` 的直接持有；
不需要删除数据库、配置或缓存。
