# ADR 0411：限制 Provider 流帧与事件队列

- 状态：已采纳
- 日期：2026-09-30
- 范围：共享 SSE / JSON-lines reader 与 OpenAI、Anthropic、Gemini、OpenAI Responses 流适配器

## 背景

共享 reader 会把响应字节不断追加到当前行，直到遇到换行；未结束的行可以无限增长。解析后的 payload 通过无界 Tokio channel 交给 provider 适配器，慢消费者也会让队列持续增长。

## 决定

- 单个 SSE 或 JSON-lines 帧最多 2 MiB，按原始字节计数；超限时立即丢弃响应 body reader，并向消费者返回明确的 `InvalidResponse`。
- 所有 provider 适配器共用容量为 4 的有界 payload channel。reader 通过异步 `send` 形成背压，队列满时暂停读取 HTTP body；消费者取消或关闭后，reader 退出并丢弃 body stream。
- EOF 时仍接受不超过上限的最后一行，保留现有 SSE/JSON-lines 解释行为。

## 替代方案

- 只限制帧大小：仍允许慢消费者积累任意多已解析事件。
- 只把 channel 改成有界：消费者停滞时，单个未换行帧仍可无限增长。
- 丢弃超限帧并继续读取：响应已失去 framing 同步，因此当前请求应整体失败。

## 影响

超过 2 MiB 的单行响应会失败；常规小帧行为保持不变。每个 stream reader 最多持有一个受限帧以及最多 4 个排队 payload，并在向队列发送时暂停 body 读取。没有数据库、配置或 IPC 契约变化，无需重置用户数据。

## 验证

- 测试无换行超限帧返回错误。
- 测试队列填满时 reader 暂停轮询 body stream，并覆盖帧大小边界。
- 运行 LLM crate 测试及适用的 workspace 格式、编译和严格 Clippy 检查。

## 回滚与重置

回滚本 ADR 与共享 reader 的帧上限、有界 channel 即可恢复原有行为；无需重置用户数据。
