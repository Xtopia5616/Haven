# ADR 0214：一次 run 在 SessionActor 任务内执行

## 状态

已接受（2026-09-23）。本决定约束后续实现；当前 `ReActState` 仍由 actor 外的循环持有，usage、stream id 和 token estimate 仍是 mailbox 命令。

本决定取代 [ADR 0196](0196-session-actor-event-sourced-state.md) 里「usage、stream identity、token estimate 经 mailbox 修改」以及「热 transcript 作为 actor 外 scratch」这两部分。[ADR 0057](0057-agent-react-state-machine.md) 的单一运行态对象仍然有效，主人改为 actor 内的 `SessionState`。

## 背景

热 transcript 没有主人。`ReActState` 仍在 actor 外面持有 events、canonical、branch points、retry nudge 和 turn_cancel。循环要把整份 canonical 复制进 mailbox，再由 actor 做 token estimate。usage 和 stream id 也是同样的内部命令。

这些缓存即使搬进 `SessionState` 也不够。循环还留在外面时，events、canonical、branch points、retry nudge、turn_cancel 以及下一层缓存会再长出来。mailbox 也会继续承载本应是函数调用的运行时读写。

## 决定

- 一次 run 在 actor 任务内执行。dispatcher 只做 admission 和外部启动、停止；它不在另一条任务上轮询 ReAct future。
- 循环只在 yield 点拿 `&mut SessionState`。yield 点是两次外部等待之间的同步段。provider、工具、数据库和计时器的 `.await` 不得握着这份借用，actor 才能在等待期间处理 mailbox。
- 热 transcript 放进 `SessionState`：events、canonical、branch points、retry nudge、turn_cancel。`ReActState` 可以保留为这个字段的类型，但不再由 actor 外的循环栈帧独占。
- mailbox 只接收外部命令：submit、steer、interaction resolve、cancel、background result、跨任务 messaging，以及 supervisor 的生命周期和快照。运行中的循环不得向自己的 mailbox 发请求再等待回复。
- usage、stream id、token estimate 是 `SessionState` 上的函数调用，不是命令。estimate 直接读 actor 已经拥有的 canonical，禁止为估算把 `Vec<CanonicalMessage>` 送过 mailbox。
- `session_events` 仍是恢复权威。热 transcript 只是进程内投影。本决定不改 schema、IPC 或 UI 事件。

## 替代方案

- 只把 token estimate、usage 或 stream id 缓存搬进 `SessionState`，循环留在 dispatcher 任务：拒绝。缓存有了主人，热 transcript 仍然没有，同类状态会再长出来，估算仍要复制 canonical。
- 把热 transcript 的每次读写都做成 mailbox 命令：拒绝。这把函数调用伪装成跨任务协议，而且 run 迁入 actor 任务后会自己等自己的回复。
- 让 run 在整个 `.await` 期间持有 `&mut SessionState`：拒绝。模型或工具等待期间，外部 submit、steer 和 cancel 进不来。
- 再给热 transcript 加一把进程内锁，让循环和 actor 并行持有：拒绝。那是两个主人。

## 影响

这条边界要一次跨过：run 在 actor 任务内被轮询，热 transcript 进入 `SessionState`，usage、stream id、estimate 改为 yield 点上的函数调用，mailbox 删除对应的内部命令。不允许先只落地缓存搬迁。

外部任务仍用 handle 发送外部命令。actor 在 run 的每次等待上 `select` mailbox，并优先处理外部命令。`RefMut` 或 `&mut SessionState` 不得留在 pending future 里。

取消仍是外部命令，并继续驱动现有 `CancellationToken`。这个 token 必须能在不借用 `SessionState` 的情况下观察，避免取消本身还要等借用释放。

## 验证

实现时至少覆盖：

- 模型等待期间的 submit、steer、cancel 仍进入 actor，且不依赖循环把 canonical 复制回 mailbox。
- usage、stream id、estimate 没有 oneshot 往返；estimate 不克隆整份 canonical。
- rollback、resume、compaction 之后热 transcript 只有 actor 内一份，revision 和 generation 不会误命中旧 estimate。
- 两个 session 的热 transcript 不共享。
- 既有 X12、确认、ask、工具顺序和 `LoopExit` 语义不变。

```text
cargo fmt --all -- --check
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked
```

## 回滚

回退实现提交即可。没有数据库、配置或 IPC 迁移。不能只回退调用方：run 所在的任务、`SessionState` 字段和 mailbox 命令必须一起回去，不能留下半套缓存。
