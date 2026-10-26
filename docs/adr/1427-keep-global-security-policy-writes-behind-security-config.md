# ADR 1427：全局安全策略仅通过完整 SecurityConfig 应用

## 状态

已采纳并实施。

## 背景

`SecurityConfig` 已统一承载新会话默认执行方式、文件与网络 `ResourcePolicy`、隔离偏好及恢复时加载的永久权限规则。跨层 `AuthorizationPort` 和 Agent 的 `ToolAuthorizationPort` 还暴露 `set_execution_mode` 与 `set_boundaries`，可以只改运行时状态的一部分；Agent adapter 也转发了这些全局变更入口。这样的入口容易让运行时策略与持久配置分叉，并且 `set_boundaries` 只能接收网络访问等级，不能替换其域名范围等完整资源策略。授权后创建或撤销权限 grant 仍通过同一 owner 的 typed grant API 完成。

## 决定

1. 全局默认执行方式、文件/网络资源边界和隔离偏好只经 `ToolsFacade::apply_security(&SecurityConfig)` 送入 Tools 组合 owner。`AuthorizationEngine::apply_security` 仍在一个写锁事务中编译和替换策略，并维护现有 revision、撤权和 fail-closed 行为。
2. `AuthorizationPort` 与 Agent `ToolAuthorizationPort` 不提供全局 `SecurityConfig` 应用、全局执行方式变更或部分文件/网络边界变更方法。全局配置更新由应用设置流程调用 Tools facade。
3. Agent 保留会话执行上下文能力：会话工作区与执行方式继续由 session event 绑定；grant、撤销和授权检查继续通过有名 typed port 操作。永久权限规则仍由该授权 owner 管理，并从 `SecurityConfig` 恢复。
4. 安全决策仍由 `AuthorizationEngine` 独占。文件、网络、进程和远程资源 adapter 各自将授权 owner 给出的类型化边界落实到真实副作用上；本决策不合并这些执行器，也不增加新的安全 crate。
5. 测试通过完整 `SecurityConfig` 配置全局策略；仅限 `haven-tools` 单元测试的局部 setter 不进入默认生产构建。

## 替代方案

- 新建独立 security crate：拒绝。当前策略的唯一写入编排、授权决策与执行证明均属于 `haven-tools`，网络/MCP 启动边界也由其 facade 协调；拆 crate 没有独立依赖边界或消费者收益。
- 按网络、文件、执行模式分别保留全局 setter：拒绝。这样会继续允许运行态策略只更新一部分，并使配置快照与实际执行边界分叉。
- 让 Agent 继续负责全局配置变更：拒绝。Agent 只拥有会话编排；全局配置由应用设置流程持久化，并交给 Tools facade 协调所有运行时边界。

## 影响与迁移

这是 Rust 内部 port 收窄，不改变配置、Tauri IPC、数据库、session event 或持久化格式，无需数据重置。Agent/App 的生产调用方不再能直接改写全局安全策略；设置保存仍由既有 Tools facade 应用完整配置。会话执行方式与工作区的 durable 所有权不变。

## 验证

- 调整 Agent/App 调用点与集成测试，使其经 `ToolsFacade::apply_security` 使用完整配置。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings` 通过。
- `cargo test --workspace --locked` 通过；Agent 689 项、App 277 项、Tools 1004 项通过，Tools 的 16 项既有忽略用例保持忽略，其它 crate、集成测试与文档测试通过。
- UI 门禁通过：Svelte 检查 0 错误/警告，Vitest 170 个文件的 1519 项测试通过，生产构建成功。
- `scripts/check-adr-index.ps1` 与 `scripts/check-crate-dependencies.ps1` 通过。策略配置测试现在经 Tools facade 使用完整 `SecurityConfig`；执行 proof 与 resource policy 行为回归通过。

## 回滚

恢复 `AuthorizationPort` 和 `ToolAuthorizationPort` 的全局写方法及对应 adapter 转发即可。没有持久数据变化；回滚不会要求配置或数据库重置。
