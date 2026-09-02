# ADR 0069：跨 session MessagingService 与统一消息生命周期

## 背景

跨 session 消息原本由 `haven-tools::inbox::InboxBus` 直接暴露给两个消费方：
`agent` 工具使用 `read_and_archive` 同步清空邮箱，ReAct 自动注入使用
`claim_and_archive` / `ack_claimed` 保留 processing 文件。这两条路径分别处理归档、崩溃恢复、
已读回执和重复投递，导致同一条消息在工具调用与 Agent 循环中具有不同的可靠性语义。

`InboxBus` 同时承担 JSONL wire format、跨进程文件锁、registry、mailbox 和消费状态；工具、
ReAct context、peer spawn 及 session 终态又各自直接访问它，消息 identity 和请求生命周期没有
一个应用层权威入口。

## 决定

1. `haven_tools::MessagingService` 成为跨 session 消息的应用层 port。`InboxBus` 只作为当前
   JSONL file transport adapter，应用代码不再直接调用它的消费生命周期方法。
2. 所有批量收件统一使用：
   `send(Envelope) → claim(recipient) → process → MessageClaim::complete()`。
   `MessageClaim` 在 `complete` 前被丢弃时保留 durable processing claim，下一次 claim 会按同一
   `msg-{uuid32}` identity 重投；`delivery_attempt` 从 1 开始递增。过期消息不进入处理，但仍
   归档供审计。`read_and_archive` 仅保留为 transport regression test helper，不再编译进运行时。
3. `MessagingService::deliver` 在写入前校验 sender、recipient、canonical message id、时间字段、
   receipt correlation 和非空正文，避免稳定业务 identity 由各个工具分支自行约定。
4. ReAct inbox auto-inject、`agent` 的 `inbox`、peer spawn/cascade 和 session 注销均通过
   `MessagingService`；`agent` 的 request/reply 仍允许 selective reply consumption，但它是
   request lifecycle 的专用操作，不是第二套普通 inbox 消费模型。
5. 现阶段保留 JSONL 文件布局，保证独立 Haven 进程可以协作；未来接入 `SessionActor` 时，替换
   service 下的 adapter，而不是在 Actor 和 JSONL 之间复制 claim/ack 状态机。spawn 的 Agent
   runtime 接线仍由现有 `AgentSpawner` port 提供，待 SessionSupervisor/mailbox 阶段再迁移，
   本 ADR 不扩大为完整 Actor 重写。

## 替代方案

- 继续让工具和 ReAct 各自调用 `InboxBus`：拒绝，无法保证 claim、重试和 receipt 的语义一致。
- 直接把 `InboxBus` 改名为 `MessagingService`：拒绝，会把文件锁和 JSONL wire 状态继续泄漏到
  应用层，未来无法替换为 in-process mailbox。
- 立即删除 JSONL transport：拒绝，当前产品仍需要独立进程间协作；本阶段先删除应用侧双轨，
  保留单一 adapter。
- 让 `agent` 工具一次性接管完整 SessionActor/supervisor：拒绝，跨 crate 生命周期和调度
  边界尚未准备好，容易把消息重构扩大成不可验证的大爆炸改写。

## 影响与验证

- JSONL mailbox/archive 路径保持不变；旧 envelope 缺少 `delivery_attempt` 时按 0 读取，首次
  claim 会写入 attempt=1，无需删除或重置用户 inbox。
- `read_and_archive` 不再是运行时兼容入口；crate 外部调用方必须迁移到 `MessagingService`。
- 消息投递仍是 at-least-once。不可幂等的消费副作用必须在应用处理层按稳定 message id 做幂等，
  `MessagingService` 不会假装提供 exactly-once。
- 负向测试覆盖 identity/routing 校验；行为测试覆盖 complete、drop/retry、attempt 递增、
  expiry/archive，以及工具和 ReAct 的 claim 链路。

重点验证：

```text
cargo fmt --all -- --check
cargo test --locked -p haven-tools messaging
cargo test --locked -p haven-agent inbox_claim_is_redeliverable_until_ack
cargo check --locked -p haven-agent -p haven-tools
cargo clippy --workspace --locked -- -D warnings
```

## 回滚

回退本 ADR 对应提交即可恢复应用侧直接使用 `InboxBus` 的旧消费路径；不会删除 JSONL mailbox
或 archive。若未来替换为 `SessionActor` mailbox，必须另立 ADR，说明跨进程 inbox 的迁移、
重试状态和用户数据处理方式。
