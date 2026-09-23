# ADR 0224：Agent ToolCatalogPort 单一读取边界

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` ReAct 对 immutable `ToolCatalogSnapshot` 的读取
- 关联：[ADR 0211](0211-operation-registry-and-platform-snapshot.md)、[ADR 0152](0152-agent-permission-boundary-refactor.md)

## 背景

ReAct 的 provider 工具目录构建、参数校验和确认恢复都直接经
`SessionSupervisor::get_tools()` 获取 `ToolsManager`，让 Agent 读取工具目录的
依赖耦合在执行 facade 上。三处读取需要对同一个 session 取 immutable
`ToolCatalogSnapshot`，但 Agent 无需依赖 snapshot 的生产者。

## 决定

1. `haven-agent` 定义 async `ToolCatalogPort`，唯一职责是按 `session_id` 返回
   `Arc<ToolCatalogSnapshot>`。`ReActEngine::new` 用
   `executor.get_tools()` 创建生产适配器；适配器只调用现有
   `ToolsManager::tool_catalog_snapshot` 并将结果包入 `Arc`。
2. provider 目录构建、输入校验和确认恢复统一通过该 port 读取。snapshot 的
   具体类型、生成与版本语义、校验逻辑、provider schema 均保持不变。
3. 该 snapshot 仅固定一次 ReAct 读取所需的目录视图；它不缓存或替代 live
   authorization。执行边界仍进行原有运行时校验和授权，执行实现与授权结果
   生命周期不变。
4. 后续按独立切片评估 `ExecutionPort`、`AuthorizationPort` 和 session
   overlay 读写 port。各切片应迁移一条完整调用链并维持现有执行、确认、装载
   与授权契约；本 ADR 不改造这些入口。

## 替代方案

- 继续让 ReAct 直接访问 `ToolsManager`：保留了 Agent 对执行 facade 的读取依赖，
  三处调用也可能各自演化。
- 同时拆分执行、授权和 overlay：扩大本切片的行为面，增加确认恢复及 session
  工具装载的变更风险，无法只验证目录读取边界。

## 影响

- ReActEngine 的 tool catalog 读取依赖收窄为 Agent-owned port；构造签名与现有
  engine 测试构造方式不变。
- 不改变工具目录内容、snapshot 版本、provider schema、工具执行、live
  authorization、持久化或 IPC 契约。

## 验证

- fake port 单测验证 session ID 原样转发及返回同一 `Arc` snapshot。
- `cargo fmt -p haven-agent -- --check`
- `cargo check --locked -p haven-agent`
- `cargo test --locked -p haven-agent`
- `cargo clippy --locked -p haven-agent -- -D warnings`
- `git diff --check`

## 回滚与重置

无持久化或配置变化，不需要数据重置。回滚时恢复 ReAct 三处经
`executor.get_tools().tool_catalog_snapshot` 的读取，并移除 port、适配器及本 ADR。
