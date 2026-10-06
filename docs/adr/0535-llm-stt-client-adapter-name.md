# ADR 0535：LLM STT 客户端转换使用 Adapter 命名

## 状态

已采纳并实施；LLM crate 门禁通过（2026-10-06）。

## 背景

`haven_llm::stt` 中的 `LlmClientSttBridge` 把 `adapter_for` 创建的 provider `LlmClient` 暴露为消费者使用的 `SttClient`。它只转发 `transcribe` 请求，没有两端共享的生命周期、消息通道或桥接状态。项目命名规范将 `Adapter` 用于稳定边界间的调用/数据转换，并把 `Bridge` 限定为既有协议或生态术语。

## 决定

1. 将私有类型 `LlmClientSttBridge` 改名为 `LlmSttClientAdapter`，并将注释改为明确描述 provider client 到 STT consumer contract 的转换。
2. 保留 `LlmClient` 与 `SttClient` 两个独立契约。它们分别服务 provider 通用调用和采集/媒体消费者，不能因本适配器只委托一个方法而合并。
3. 保持 provider dispatch、超时、错误传递与转写结果行为不变。

## 替代方案

- 保留 `Bridge`：拒绝。该类型没有桥接状态或异步通道，`Adapter` 更准确地表达其职责。
- 让每个 STT provider 直接实现 `SttClient`：拒绝。provider 实现已经统一复用 LLM adapters；重复包装会分散协议和认证实现。

## 影响与验证

- 仅重命名 `haven-llm` 私有实现并更新当前架构审计状态。
- 不改变 IPC、MCP wire、配置、持久化或用户文案，无需数据重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --locked -p haven-llm`、`cargo clippy --locked -p haven-llm -- -D warnings`、`cargo test --locked -p haven-llm -- --test-threads=1`（508 passed，1 个手工性能测试 ignored）。测试使用执行前不存在的隔离 `APPDATA` 根目录 `target/audit-runtime-data-0535/AppData/Roaming`。

## 回滚

恢复私有类型名 `LlmClientSttBridge` 和原注释，并同步恢复本 ADR 与路线图条目。无持久化回滚或数据重置。
