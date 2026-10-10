# ADR 0881：将 LLM mock router API 限定到测试依赖

## 状态

Accepted — 2026-10-10

## 背景

`LlmRouter::new_with_clients` / `new_with_clients_full` 使用固定测试路由与注入 client；`force_request_configured` / `force_request_primary_for_test` 会直接改写请求路由及凭据就绪状态。调用图显示这些入口只被 LLM、Agent 和 Tools 的测试使用，却位于正常 `haven-llm` 构建中。尤其 `force_request_configured` 会把请求切到注入路由或 production route，适合模拟门控，不应成为正常运行时控制面。

跨 crate 测试不能使用依赖库的 `#[cfg(test)]` 项，因此仅加该属性会使 Agent/Tools 测试无法构建。Haven 没有为这些内部测试 helper 保留向后兼容的要求。

## 决定

- 将 mock router 构造器命名为 `new_with_test_clients` 与 `new_with_test_clients_and_embedding`；私有配置工厂命名为 `injected_client_test_config`。
- 将路由状态控制命名为 `set_request_configured_for_test` 与 `set_request_primary_for_test`，并与构造器一起限定在 `cfg(test)` 或 `test-support` feature。
- 为 `haven-llm` 增加非默认 `test-support` feature；只由 Agent 与 Tools 的 dev-dependency 启用。生产依赖默认不编译这些测试 API。
- LLM 自身单元测试通过 crate 内 `cfg(test)` 使用同一入口；保留生产 `LlmRouter::new` 和真实 Settings/Router config apply 路径。

## 影响与兼容性

只收回测试注入与状态伪造入口，不改变生产路由、认证、配置、provider 调用或 IPC 行为。显式启用 `test-support` 的下游测试可继续构造注入路由；正常依赖图不含该 feature，无需数据或配置重置。不提供旧构造器或方法的兼容 alias。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --all-targets --locked`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 与 `git diff --check`。全目标编译覆盖启用测试 feature 的 Agent/Tools 测试目标，但没有执行测试套件。

## 回滚

若未来生产场景需要运行时注入 client 或动态改写路由，应定义独立且受配置/授权约束的生产 API；不要重开测试状态伪造入口或默认启用 `test-support`。
