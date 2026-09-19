# ADR 0173：恢复持久化与 provider 失败边界

## 背景

失败的流式模型回合同时拥有 `partial_messages` scratch、恢复消息、branch point
和 recovery snapshot。旧路径在写入失败后仍会丢弃 scratch，导致 Continue 既没有
可见消息也没有恢复标记。模型路由也已经支持候选列表，但熔断器仍按 legacy role
共享，主模型失败会污染备用模型的健康状态。

## 决定

- 恢复路径返回结构化结果；只有 branch point、恢复消息/投影和 recovery snapshot
  全部成功时才允许 `PartialStore::discard`。失败时保留 scratch，并记录明确的
  recovery persistence failure。
- 路由熔断与半开探测按 routed model id 隔离；role 继续只承载并发 semaphore 和
  rate-limit pacing。候选模型按顺序跳过 open breaker。
- 所有非流式 provider、OCR、TTS 和 MCP 成功响应读取都检查 `Content-Length`，并
  对 chunked body 在消费过程中执行同一上限；MCP stdio 行和 notification queue
  也采用有限容量。`tools/list_changed` 不进入可丢弃的普通通知队列，而由单独的
  coalesced invalidation signal 驱动缓存刷新。
- MCP HTTP 请求把响应头等待与有限 body/notify drain 视为两个独立 deadline；
  长连接 SSE 仍由其专用生命周期控制。通知 listener 同时受 client 生命周期取消
  和稳定的 listener shutdown token 保护，连接重试不会遗失 listener，明确 shutdown
  后也不会留下轮询任务。
- failover 仅用于明确的 transport/server/rate-limit 类失败；认证、配置、能力、
  普通 4xx 和用户输入语义错误不再盲目广播到所有候选。`Retry-After` 支持
  HTTP-date，并受本地上限约束。
- HTTP 408 映射为可重试的 `Timeout`，409 保留为不可 failover 的
  `RequestFailed`，425 映射为可重试的 transient `ServerError`；`InvalidResponse`
  与 `Unknown` 明确不触发 provider failover；health-check 复用同一 HTTP 状态
  分类，不再把非 401/403 状态统一伪装成 `ServerError`。
- MCP monitor backoff、rate-limit pacing、stdio request/notify 写入和读取均观察
  cancellation/timeout。流式 flush 必须等待 watchdog，且 checkpoint 数据库失败
  通过 `PartialStore` 传播到 flush 结果。

## 影响与回滚

本次不改变数据库 schema、snapshot wire shape 或 provider 配置，不需要用户重置。
回滚代码即可恢复旧运行时行为；已有 scratch 行仍可由新版本继续处理。

## 验证

- `cargo test --locked -p haven-llm --lib`
- `cargo test --locked -p haven-mcp --lib`
- `cargo test --locked -p haven-agent --lib partial::tests::checkpoint_database_failure_is_returned_to_flush`
- `cargo check --locked -p haven-agent`
- `cargo check --locked -p haven-mcp`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked`
- `cd ui && corepack pnpm run check && corepack pnpm run test:run && corepack pnpm run build`
