# ADR 0098：LLM 流式请求建立阶段可取消

## 背景

输入区的“中断输出”已经通过 `interrupt_session` 取消会话令牌，但流式
provider 请求在返回响应头之前仍处于 `chat_stream_with_tools_output_cap(...).await`。
该等待不在取消选择分支内，因此网络连接建立或 provider 首包响应头迟迟不到时，
用户按下中断后仍可能要等传输超时才能结束。

## 决定

1. 将 provider 流的创建/响应头等待纳入 `CancellationToken` 的优先取消分支。
2. 将流式路由等待并发 permit 与限流冷却的阶段也纳入同一取消分支；取消后丢弃
   整个请求 future，避免暂停会话在之后才占用 provider 请求槽位。
3. UI 在 IPC 返回前立即进入“正在停止”状态，防止重复点击；会话仍保持可继续，
   “结束会话”继续走独立的终止路径。

## 替代方案

- 只缩短 provider transport timeout：会让停止更快但仍不是取消，且会影响正常慢请求。
- 只在已有 stream 消费循环中检查令牌：无法覆盖响应头尚未返回的阶段，拒绝。

## 影响

正常请求的 provider 协议和超时策略不变；取消时 reqwest 请求 future 会被丢弃，
已有的暂停、快照和后续继续逻辑保持不变。无数据库、配置或 IPC 契约变更。

## 验证

- `cargo test --locked -p haven-llm -- streaming::tests::cancellation_interrupts_provider_stream_creation`
- `corepack pnpm --dir ui run check`
- `corepack pnpm --dir ui run test:run`

## 回滚 / 重置

回滚本 ADR 对应代码即可恢复原行为；不涉及用户数据重置。
