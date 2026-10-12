# ADR 1044：用稳定消息身份确认 Agent inbox

## 状态

已接受（2026-10-12）

## 背景

显式 `agent.inbox` 曾生成进程内 `claim-*`，将其与消息一起写入 ToolResult。ToolResult、工具参数和结果都会进入 durable transcript，因此重启后历史中仍能看到 token，但新建的 `AgentTool` 没有对应的内存 claim map，旧 token 无法确认消息。现有 `message_ids` 路径已经支持按稳定消息身份选择性 ack。

## 决定

- `agent.inbox` 只返回稳定的 `message_ids` 与消息内容，不再生成或暴露 `claim-*`。
- `agent.ack` 只接受 `message_ids`，每次最多 100 个；更大的 inbox 批次分多次 ack。未选择的消息保留在 processing 状态并继续可领取。
- `AgentTool` 不再持有跨工具调用的 process-local `MessageClaim` map。inbox 返回后，底层 transport 仍按其既有生命周期保留未确认消息。
- 旧 transcript 中的 `claim_token` 不能代表当前领取。typed input 暂时保留该历史字段，仅用于返回明确迁移错误；模型 schema、prompt、新 ToolResult 均不再包含它。

## 替代方案

- 将领取令牌做成 durable 记录并提供恢复协议：会多引入一套持久 claim 身份和生命周期；已有 `msg-*` 足以精确确认，故不采用。
- 保留 token 并在重启后重新绑定：无法安全证明旧 token 对应当前仍有效的消息批次，且会继续把临时 runtime 身份写进 transcript。

## 影响与兼容

Tool schema 与模型可见结果发生有意变化，但 session event 和消息持久化 shape 不变，无需数据库重置。历史 transcript 保留原结果；遇到旧 token 调用时返回迁移提示，用户/模型重新读取 inbox 并使用其中稳定的 `message_ids`。文件 JSONL transport 在进程重启后仍以原 `msg-*` 重投；此 ADR 不扩展或改变其它 transport 的崩溃耐久性。

## 验证

- 测试 inbox 结果只公开 `message_ids`，并验证新的 `AgentTool` 实例可用这些 ID 确认原文件 transport processing 消息。
- 测试旧 canonical `claim_token` 返回明确的 `message_ids` 迁移提示，畸形 token 仍在副作用前被拒绝。
- 运行受影响 crate 与 workspace 规定的 locked 编译、严格 Clippy 和测试门禁。
