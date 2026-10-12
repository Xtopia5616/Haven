# 1023：收回仅限测试和 crate 内部使用的 API

## 状态

已采纳并实施（2026-10-12）。

## 背景

全仓调用图复核发现，少数类型、辅助函数和构造入口虽然是公开 crate API，但没有跨 crate 生产消费者；其中 `InMemoryCredentialStore` 仅用于隔离测试，不应进入默认生产构建。公开可见性会扩大稳定契约，也可能让调用方绕过各自 owner。

## 决定

- `haven-common::config::InMemoryCredentialStore` 仅在单元测试或显式启用 `test-support` feature 时编译。`haven-tools` 和 `haven-app-binary` 只在各自开发依赖中启用该 feature；Common 的普通生产依赖不启用测试替身。
- Input 的 `vad_state` 仅供本模块使用；MCP 的显式网络策略构造器仅供 crate 内部使用，网络策略读取器收窄为 `pub(crate)`。
- LLM 的请求客户端选择 helper 仅在测试中编译；registry fallback 常量保留 crate 内可见性；模型 JSON parser 不再由 crate 根导出；仅供 registry 单测使用的 `all` / `search` helper 仅在测试构建中编译。
- Tools 的输出实现类型和模型工具名转换 helper 收窄为 crate 内 API。仍有跨 crate 生产消费者的 `skill_tool_name`、`ModelRegistry`、`ModelInfo` 与 context-window 查询保持公开。
- `LlmRouter::config` 仍由 Agent/Tools 生产路径消费，返回完整配置的边界另行审计，不在本切片中仓促收窄。

## 影响

不改变运行时行为、IPC、事件、持久化或配置格式。普通生产依赖不再获得无消费者的实现入口；测试仍可通过 `cfg(test)` 或非默认 `test-support` 构建覆盖替身与 helper。

## 替代方案

- 保留当前公开 API 并只补注释：拒绝，调用图已确认这些入口没有外部生产消费者，继续公开只会扩大契约面。
- 将测试替身放进默认 Common 构建：拒绝，会让仅用于测试的进程内凭据存储进入生产 API。
- 直接收回 `LlmRouter::config`：拒绝，当前 Agent 和 Tools 仍有生产消费者；需要先设计窄配置查询或安全 DTO 并迁移完整调用链。

## 验证

- 全仓引用检索确认收窄入口没有未迁移的跨 crate 生产调用。
- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo check --workspace --locked`（无测试 feature 的生产依赖图）
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1` 与 `git diff --check`

## 回滚

若以后出现真实跨 crate 消费者，应先定义该领域的窄契约，再单独开放所需入口；无需数据库、配置或用户数据重置。
