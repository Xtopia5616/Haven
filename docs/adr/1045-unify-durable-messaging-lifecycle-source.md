# ADR 1045：统一 Messaging 的持久生命周期来源

## 状态

已接受（2026-10-12）

## 背景

ADR 0069、0158、0396 将跨 session MessagingService 接入 SessionActor mailbox，目标是让本地投递及时唤醒。实际实现由 SessionSupervisor 按目标 session 是否有本地 Actor 在内存队列与 JSONL 文件之间选择；claim、ack、历史和回复查询又分别按当前 Actor 是否存在路由。由此产生两份生命周期状态：本地 Actor 队列在进程重启时丢失，外部 JSONL 消息可能被本地空队列遮蔽，Actor 创建/移除还可能让同一消息的 claim 与 ack 走不同来源。

JSONL transport 已支持稳定 `msg-*`、processing/ack、重投、历史与选择性回复，且本地和独立进程均能访问同一数据目录。Actor 的职责只需保留消息轮询提示，不需要持有消息实体。

## 决定

- JSONL transport 是本地 session 与独立进程共享的唯一消息生命周期来源；投递、claim、try-claim、ack、last-received、按 ID 查询、历史和回复查询均直接访问该 transport。
- 移除 `SessionMailbox` API、SessionSupervisor 的 Actor 路由，以及 SessionActor 内存 inbox、processing、archive 和对应命令。SessionActor 只保留轮询游标、轮询节拍与标题缓存等运行提示。
- MessagingService 的 runtime port 只处理 peer session spawn/control，不再提供消息存储或路由能力。多个生产 MessagingService 实例使用同一默认 JSONL 根和进程内通知器。
- 未确认的 JSONL processing 消息在应用进程重启后以原 `message_id` 重投。该实现通过文件 flush 写入，不据此承诺突然断电时的 `sync_all` 级耐久性。
- Agent 单测的 SessionSupervisor 使用独立临时 JSONL 根，避免测试访问用户数据目录；独立 transport 实例共用该根以验证进程边界行为。

## 替代方案

- 保留 Actor inbox 并为它增加独立持久化：会建立第二种消息状态和恢复/去重协议，仍需解决 JSONL 消息如何导入及 ack 所有权，故不采用。
- 只把本地投递改为 JSONL、继续由 Actor 优先 claim：会继续遮蔽外部消息并保留随 Actor 生命周期切换 ack 来源的可能，故不采用。
- 将通知也持久化：通知只是唤醒提示，消费者已有周期性 claim；将提示持久化会重复表达 JSONL 中的可领取状态，故不采用。

## 影响与兼容

新增消息只写入 JSONL。JSONL envelope、registry、工具输入输出与 Session event schema 不变，不需要数据库重置。升级时旧版本内存 Actor 队列中尚未投影的消息不能从文件恢复；这是原实现已有的进程重启丢失窗口。新版本的未确认 JSONL 消息按稳定 ID 恢复。ADR 1044 关于 `claim-*` 的历史迁移说明继续适用；本决策进一步统一其所依赖的 transport 来源。

## 验证

- 在本地 Actor 存在时，由独立 service 写入 JSONL，再由 supervisor 的 service claim，确认本地 Actor 不会遮蔽外部写入。
- 丢弃未完成的 claim，移除接收 Actor，再以新的 transport/service 实例重领，确认 delivery attempt 增长且 `message_id` 不变；后续 ack/receipt/reply/history 沿相同 transport 完成。
- 删除 SessionMailbox 和 Actor 内 inbox 之后，搜索 crate 与当前架构文档，确认不存在第二条应用层路由。
- 运行受影响 crate 与 workspace 规定的 locked 编译、严格 Clippy 和测试门禁。
