# ADR 1095：限制 Messaging durable envelope 写入

## 状态

已接受（2026-10-13）

## 背景

Messaging 工具原有 text、subject 与 payload 字段上限只在工具层执行。`MessagingService::deliver` 与 JSONL `InboxBus::deliver` 可被直接调用，因而能绕过这些边界；payload 上限检查还先把整个 JSON 序列化为临时 `Vec`。时间字符串也没有长度边界，完整 envelope 的 JSON 行因此没有明确上限。

## 决定

- 将既有上限作为 `haven-messaging` 的契约常量：text 16 KiB、subject 512 B、序列化 payload 64 KiB。Tools 的输入校验和 schema 复用这些常量。
- 在 `MessagingService` 与 `InboxBus` 的写入入口调用同一个 delivery validator。RFC3339 时间字符串最多 35 UTF-8 字节；完整 envelope 的 JSON 大小由字段上限及固定 framing 推导，并通过有界计数 writer 校验。
- payload 与完整 envelope 的序列化校验不保留序列化副本。超限写入在追加 JSONL 前失败。
- 已持久化消息仍使用结构校验读取，避免新增写入限制使旧的未确认消息无法领取。JSONL shape、消息 ID 与数据库 schema 均不变，无需重置 inbox。
- 本 ADR 不为 mailbox/`.processing` 增加累计容量，也不改变 claim/ack 生命周期。累计 pending 容量与旧 backlog 的有界读取继续作为路线图待办。

## 验证

- 在 `haven-messaging` 测试 exact field limits、完整 JSON 预算、时间上限以及 `MessagingService`/`InboxBus` 直接写入的拒绝路径。
- 在 `haven-tools` 测试共享字段边界及多字节 UTF-8 输入。
