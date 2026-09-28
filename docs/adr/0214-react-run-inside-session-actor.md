# ADR 0214：一次 run 在 SessionActor 任务内执行

## 状态

已接受并完成实现（2026-09-28）。`SessionActor` task 持有完整 `SessionState`；其中 `react_run` 槽持有 active run future，future 独占捕获该 run 的 `ReActState`，并与外部 mailbox 一起由同一 actor loop 轮询。热状态不会由 actor 外的 ReAct loop 或另一个 session 持有。usage、stream identity 与 token estimate 的后续 mailbox 收口见 ADR 0222、0223、0276、0278。

本决定取代 [ADR 0196](0196-session-actor-event-sourced-state.md) 里「usage、stream identity、token estimate 经 mailbox 修改」以及「热 transcript 作为 actor 外 scratch」这两部分。[ADR 0057](0057-agent-react-state-machine.md) 的单一运行态对象仍然有效，主人改为 actor 内的 `SessionState`。

## 背景

热 transcript 没有主人。`ReActState` 仍在 actor 外面持有 events、canonical、branch points、retry nudge 和 turn_cancel。循环要把整份 canonical 复制进 mailbox，再由 actor 做 token estimate。usage 和 stream id 也是同样的内部命令。

这些缓存即使搬进 `SessionState` 也不够。循环还留在外面时，events、canonical、branch points、retry nudge、turn_cancel 以及下一层缓存会再长出来。mailbox 也会继续承载本应是函数调用的运行时读写。

## 决定

- 一次 run 在 actor 任务内执行。dispatcher 只做 admission 和外部启动、停止；它不在另一条任务上轮询 ReAct future。
- `SessionState::react_run` 持有 active run future，future 独占捕获 events、canonical、branch points、retry nudge、turn_cancel 所在的 `ReActState`。run future 不借用整个 `SessionState`；provider、工具、数据库和计时器等待期间，actor 仍可处理 mailbox 并更新会话队列与生命周期字段（实现见 ADR 0382）。
- actor 内的同步状态转换直接操作 actor-owned state；ReAct run 通过捕获的 run-local `ReActState` 推进，不把热 transcript 复制到 mailbox，也不让 pending future 锁住整份 `SessionState`。
- mailbox 只接收外部命令：submit、steer、interaction resolve、cancel、background result、跨任务 messaging，以及 supervisor 的生命周期和快照。运行中的循环不得向自己的 mailbox 发请求再等待回复。
- usage、stream id、token estimate 不通过 mailbox 往返：usage 归 `UsageRuntime`，stream identity 与 token estimate 归 run-local `ReActState`。estimate 直接读取同一 ReActState 的 canonical，禁止为估算把 `Vec<CanonicalMessage>` 送过 mailbox。
- `session_events` 仍是恢复权威。热 transcript 只是进程内投影。本决定不改 schema、IPC 或 UI 事件。

## 替代方案

- 只把 token estimate、usage 或 stream id 缓存搬进 `SessionState`，循环留在 dispatcher 任务：拒绝。缓存有了主人，热 transcript 仍然没有，同类状态会再长出来，估算仍要复制 canonical。
- 把热 transcript 的每次读写都做成 mailbox 命令：拒绝。这把函数调用伪装成跨任务协议，而且 run 迁入 actor 任务后会自己等自己的回复。
- 让 run 在整个 `.await` 期间持有 `&mut SessionState`：拒绝。模型或工具等待期间，外部 submit、steer 和 cancel 进不来；active future 不借用整份 state，因此 actor 可以继续服务 mailbox。
- 再给热 transcript 加一把进程内锁，让循环和 actor 并行持有：拒绝。那是两个主人。

## 影响

这条边界要一次跨过：run 在 actor 任务内被轮询，热 transcript 随 `SessionState::react_run` 的 future 由 actor 持有，usage、stream id、estimate 由各自唯一 owner 管理，mailbox 删除对应的内部命令。不允许先只落地缓存搬迁。

外部任务仍用 handle 发送外部命令。actor 在 run 的每次等待上 `select` mailbox，并优先处理外部命令。active run 不借用 `SessionState`，因此它挂起时 actor 仍可修改队列、交互和生命周期状态；其 `ReActState` 只由该 future 独占访问。

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
