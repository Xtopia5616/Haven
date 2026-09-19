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
  也采用有限容量。
- failover 仅用于明确的 transport/server/rate-limit 类失败；认证、配置、能力、
  普通 4xx 和用户输入语义错误不再盲目广播到所有候选。`Retry-After` 支持
  HTTP-date，并受本地上限约束。

## 影响与回滚

本次不改变数据库 schema、snapshot wire shape 或 provider 配置，不需要用户重置。
回滚代码即可恢复旧运行时行为；已有 scratch 行仍可由新版本继续处理。

## 验证

- `cargo test --locked -p haven-llm --lib`
- `cargo check --locked -p haven-agent`
- `cargo check --locked -p haven-mcp`
- `cargo clippy --workspace --locked -- -D warnings`
