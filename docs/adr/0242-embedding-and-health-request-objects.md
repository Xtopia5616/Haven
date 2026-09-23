# ADR 0242：Embedding 与 health 请求对象

- 状态：已采纳（2026-09-24）
- 范围：`haven-llm` embedding 与 health_check Router 入口，以及三个 Agent/Tools 调用点
- 关联：[ADR 0234](0234-llm-complete-request-object.md)、[ADR 0241](0241-aggregated-stream-request-object.md)

## 背景

Embedding 批次和 health route 由位置参数跨越 crate 边界，调用方与 Router 容易对参数含义及顺序产生漂移。embedding 输入是一次逻辑请求的完整拥有数据；health 检查则只需一个 `RequestKind` 来选择 Router 已配置的路由。

## 决定与事实所有权

1. 增加纯数据 `EmbeddingRequest { input: Vec<String> }` 与 `HealthCheckRequest { request: RequestKind }`，并令 `LlmRouter::embed`、`LlmRouter::health_check` 分别接收对应对象。
2. `EmbeddingRequest` 拥有完整批次，使 Router 的异步重试闭包能够安全复用同一份有序输入；它不包含路由、重试、usage 或 provider 状态。`HealthCheckRequest` 将路由选择作为显式请求数据表达，不复制 Router 的 endpoint 或健康状态。
3. 调用方负责构造业务请求数据；`LlmRouter` 仍是 route、permit、cooldown、model-keyed circuit、重试、超时、usage 与 adapter 调用的唯一运行时 owner。request object 不承载执行策略或运行时状态。
4. `embed_text` 在 Router 内构造 `EmbeddingRequest`；`connection_status` 和 `prewarm_all` 在 Router 内构造 `HealthCheckRequest`。跨 crate 只迁移 memory index 批量 embedding、Agent router 预热和 Tools 管理诊断三个生产调用点。

## 必须保持的不变量

- 空 embedding 批次在路由解析、permit 获取和 provider 调用前短路，返回空 vectors、`model: None` 和默认 usage；非空但未配置的 embedding route 仍返回 `Configuration`。
- embedding 的输入顺序、重复值及原始字符串不变；既有 embedding route、permit、重试预算、总超时、usage、adapter 和成功/失败健康记录路径保持不变。
- health check 仍经 `with_request_permit`，使用所选 model ID 对应的 semaphore、cooldown 和 circuit breaker，并保留原有总超时及成功/失败/rate-limit 记录。
- `connection_status` 对未配置或 credentials 未就绪的 endpoint 继续返回 `Unconfigured` 且不发网络请求；已配置 endpoint 仍通过同一 health check。管理诊断仍先判断 configured 并报告 `not_configured`；`prewarm_all` 仍跳过未配置 route，失败时只额外重试一次。

## 暂缓项

不在本 ADR 内拆分 Router executor、改变 `RequestKind` 的 capability/call-purpose 职责、修改 provider adapter/wire contract、改造 raw stream 生命周期，或调整 embedding/health 的重试、usage 与熔断管线。以上工作须有各自的行为不变量和独立切片。

## 验证与回滚

验证覆盖 DTO 载荷原样到达 embedding adapter、空批次短路、健康检查已配置成功及未配置语义；此外运行格式检查、`haven-llm`/`haven-agent`/`haven-tools` 测试和 workspace 严格 Clippy。

回滚时恢复两个 Router 方法的旧参数形式并还原三个生产调用点；此切片不改数据库、配置持久化或 provider wire 数据，无 schema 重置要求。
