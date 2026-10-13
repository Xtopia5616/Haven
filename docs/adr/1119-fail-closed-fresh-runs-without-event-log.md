# ADR 1119：无事件流时限制会话投影恢复

## 状态

已接受（2026-10-13）

## 背景

`session_events` 是已提交 transcript 的唯一恢复权威；没有事件流时，Agent 走 fresh-run 路径。该路径在补充输入的 `UserInject` 尚未提交时，会从 `pending_session_inputs` 重新排队，但同一消息仍留在 `messages` 投影里。fresh-run prompt history 原先也读取这行，导致模型同时从系统历史和队列注入收到同一条输入。相同缺口还允许缺失事件流的会话带着其它 transcript 投影行继续运行，形成无法按事件顺序恢复的历史。

## 决定

- fresh-run prompt history 查询排除带有 `pending_session_inputs` 标记的消息；pending marker 仍是恢复该消息唯一的待投递记录，不依赖文本比较。
- 没有 durable event stream 时，仅允许初始用户消息投影和明确标记的待投递输入。若存在其它消息投影，Agent fail closed 并要求修复/重置，而不把投影当成可恢复 transcript。
- 事件流存在时仍只按 sequence replay；不加载 prompt-history 投影，也不从投影补写事件。

## 替代方案

- 按文本或时间猜测哪些投影已经 delivery：文本去重会吞掉合法的重复输入，时间推断也不具备提交身份。
- 继续让任意 message projection 通过 fresh-run 路径：会把不具备 durable event 顺序的 transcript 误当成完整会话。

## 影响与验证

- 不改变 schema、持久字段、事件 payload 或 IPC；无需数据库重置。
- Memory 测试覆盖 pending 消息不进入 prompt history，也不挤占 history limit；Agent 恢复测试覆盖待投递消息只出现一次，并拒绝无事件流的非初始 transcript 投影。
- 验证：Rust workspace 格式、测试、编译与严格 Clippy 门禁；UI 检查和完整测试；ADR 索引检查。

## 回滚

回滚需同时撤销 pending history 过滤、无事件流投影检查、对应回归测试和本 ADR。没有持久化迁移。
