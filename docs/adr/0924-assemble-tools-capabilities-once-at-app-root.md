# 0924：在唯一 App composition root 装配 Tools capabilities

## 状态

已接受并实现（2026-10-11）。

## 背景

ADR 0922 已要求 `share_services()` 只作为 composition-time handoff，但 AppState 建 Agent ports、读取 LLM 的 MCP caller、以及 `ApplicationRuntime::new` 组装 App service adapters 时分别从 `ToolsFacade` 读取整份 `ToolServices`。这些都是启动期调用，没有运行期 service-locator 问题，但 App 的实际 composition root 仍有多个 bundle 获取点，容易让未来消费者绕过显式依赖参数自行拉取能力。

## 决定

- AppState 创建 `ToolsFacade` 后只调用一次 `share_services()`。
- `AppState::new` 是唯一 App composition root；`ApplicationRuntime` 是接收已装配服务并管理 app-scoped task/service 生命周期的 owner。
- Agent adapter factory 接收 `ToolsFacade` 的执行/目录 façade，以及 authorization、Skills catalog、ToolRun Agent 三项已选定 capability；不自行获取 bundle。
- `RuntimeServices` 明确接收同一 `ToolServices`，`ApplicationRuntime::new` 直接适配其 App-facing ports，不再访问 `ToolsFacade::share_services()`。
- 测试专用的 facade helper 可在 `cfg(test)` 下从 facade 获取 capabilities，生产 composition API 保持显式。

## 影响

App composition 具有单一 bundle 读取点，运行时构造器和 Agent adapter 的依赖可见于参数列表。能力实现、owner、生命周期、授权、MCP 路由、IPC、配置和持久化行为不变；没有数据迁移或重置要求。

## 验证

- App binary tests，覆盖 adapter、启动装配与各 AppState 测试构造。
- 全 workspace format、check、test 和严格 Clippy。
- 静态调用图确认生产 `share_services()` 只在 AppState composition root 出现，其他直接取 bundle 的 helper 只在 `cfg(test)` 下使用。
- ADR 索引检查。

## 替代方案

- 维持每个 adapter/owner 各自读取一次 bundle：拒绝，构造依赖仍由 facade 隐式供应，composition root 的实际交接路径分散。
- 为 `ToolServices` 再加一个 App wrapper bundle：拒绝，现有类型已经是所需的一次性交接，额外包装只会重复字段。
- 把 bundle 保存在运行时并允许消费者随时读取：拒绝，消费者应只持有各自选择的窄 port。

## 回滚

恢复 agent adapter factory 和 ApplicationRuntime 从 ToolsFacade 取 bundle，并移除 `RuntimeServices.tool_services` 字段即可。无外部接口、持久化或 wire 变化。
