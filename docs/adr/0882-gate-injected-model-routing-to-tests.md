# ADR 0882：将注入式模型路由设施限定到测试依赖

## 状态

Accepted — 2026-10-10

## 背景

ADR 0881 将 `LlmRouter` 的 mock client 构造器与路由状态改写入口移至非默认 `test-support` feature。沿完整调用链复核后发现，`ModelDirectory::with_injected_clients`、`rebuild_primary_routes` 和 `RouteMode::InjectedClients` 仍然在普通生产构建中可用，尽管生产路由只由应用配置构建，仓内也没有生产调用者。这留下了可绕过 provider 凭据就绪检查的内部路线。

## 决定

- 为注入式目录构造、注入路由模式和测试配置下的路由表重建使用与 ADR 0881 相同的 `cfg(test)` 或 `test-support` 条件。
- 普通生产依赖只编译 `RouteMode::Production`，并由 `ModelDirectory::from_config` 根据已配置模型、凭据和 capability 建立 primary route。
- 保留 LLM crate 自身单元测试和显式启用 `test-support` 的 Agent/Tools 测试路径。

## 影响与兼容性

生产路由行为不变；正常构建不再包含无生产消费者的凭据绕过模式。无需数据、配置或 IPC 重置，不提供旧设施的兼容入口。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --all-targets --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、ADR Prettier 检查与 `git diff --check`。全目标编译覆盖 LLM 单测和 Agent/Tools 的测试 feature，但没有执行测试套件。

## 回滚

如果生产确实需要注入 client，应先设计带明确授权和凭据语义的生产路由入口；不要启用测试路由模式代替生产配置。
