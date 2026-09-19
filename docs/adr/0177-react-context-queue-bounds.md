# ADR 0177：ReAct 上下文队列容量与批量注入边界

- 状态：accepted
- 日期：2026-09-19
- 范围：`haven-agent`、`haven-tools`、`haven-memory`
- 关联：ADR 0159、0176

## 背景

steering、follow-up、后台 action result 和跨 session inbox 都会在模型调用前
进入同一条 ReAct transcript。此前 actor 队列没有 payload 预算，turn-start 会一次
性 take 掉所有项目，并且 inbox claim 的确认边界容易与 snapshot checkpoint 脱钩。
长文本、附件或 burst 输入因此可能无界增长；snapshot 写失败时也必须保留可重投的
inbox claim。

## 决定

1. process-local steering/follow-up/action-result 队列分别限制 item 数、字符数、每项
   附件数、单附件字节数和队列附件字节数。超过限制返回显式 back-pressure 错误，用户
   ingress 的已持久化 message 保留在 durable messages 中，不截断、不静默丢弃。
2. 一个 turn 只 drain 有界 FIFO 前缀；steering 非空时 follow-up 继续留在队列中，剩余
   action result 也保留到后续 turn。队列计数随入队、claim/drain、清理同步更新。
3. context injection 使用 `TranscriptBatchWriter` 一次 append 事件批次，随后按同一顺序
   投影 canonical、thought step、UI supplement 和 media-plan；事件 authority 仍是
   `session_events`，messages/session_steps/snapshot 仍是投影或缓存。
4. ReAct state 在加载时建立 user-inject message-id 索引，并在 append/compaction 时维护；
   inbox 重投不再每次扫描完整 `state.events`。
5. inbox claim 只确认已经投影且 snapshot durable 的选中 envelope id。超出本轮 item/
   字符预算的尾部留在 processing 文件，snapshot 失败则整个 claim 留待 at-least-once
   重投；不使用截断来掩盖超限输入。
6. session actor 的 inbox archive 使用有界 FIFO 保留最近的审计尾部；active/archive
   message-id 集合负责 O(1) 去重，淘汰 archive 头部时同步释放对应的 dedupe id。

## 替代方案

- 直接截断超限文本或附件：拒绝，会静默改变用户输入。
- drain 后在失败时丢弃队列：拒绝，无法证明 X12 投影已 durable。
- 每次从 `state.events` 重建去重集合：拒绝，长会话每次 inbox 重投都会产生线性扫描。
- claim 后无条件 ack：拒绝，snapshot checkpoint 可能尚未 durable。

## 影响与验证

这是跨 `haven-agent`/`haven-tools` 的运行时契约变更，无数据库 schema 迁移；durable
inbox processing 文件和 append-only `session_events` 仍可按原语义恢复。覆盖队列 item/
字符/附件边界、steering 优先级、高并发入队、FIFO 尾部延后、批次 event 顺序、选中
id ack、bounded archive/dedupe、snapshot 失败重投与 at-least-once claim。

## 回滚

回退对应代码和本 ADR 即可，无需数据库重置；保留的 processing 文件会继续按旧 claim/
ack 语义 redeliver。
