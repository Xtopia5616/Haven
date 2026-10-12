# ADR 1100：不在生产 ToolsFacade 暴露可执行工具句柄

## 状态

已接受（2026-10-13）

## 背景

`ToolsFacade::execute_tool` 是安全执行入口，集中执行授权、schema 校验、circuit gate、重试与取消策略；公开的 `get_tool` 却能返回 `ToolHandle`，使调用者拿到工具后直接执行并绕开这些策略。仓库中的 facade getter 消费者只用于集成测试的注册断言。

## 决定

- `ToolsFacade::get_tool` 仅在 `test` 或 `test-support` 构建中公开，供跨 crate 集成测试检查注册状态。
- `AuthorizedExecutor` 的 getter 收窄为 crate 内可见；生产调用者必须通过 facade 执行。
- 业务查询能力应通过受授权的 typed facade/operation 提供，不能重新导出 raw `ToolHandle`。

## 影响

生产 crate 外的调用者不能再通过此 getter 直接检查安装工具。测试依赖启用 `test-support` feature；工具执行行为保持不变。

## 验证

使用应用与 Agent 现有注册断言确认 test-support 路径可用；生产构建检查确保 getter 不进入默认 API。

## 回滚

不得恢复生产环境的 raw handle getter。若需要生产查询，应新增不具备执行能力的注册信息 DTO。
