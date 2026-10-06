# ADR 0536：Tools 对外执行入口使用 Facade 命名

## 状态

已采纳并实施；Rust workspace 门禁通过。

## 背景

`haven_tools::ToolsManager` 的类型文档已将它定义为工具执行 façade。它聚合 ToolRuntimeCoordinator 和进程服务 bundle，把执行、授权、目录投影、配置应用、媒体资产 lease 与录音转写入口组合给 Agent/App；MCP connection、Skills venv、ToolRun 等资源的创建和生命周期仍由各自的 owner 负责。类型本身不承担名称 `Manager` 通常暗示的资源创建/替换/重连生命周期。

Agent 通过多个窄 port 消费者 adapter 使用该组合入口；App composition root 创建唯一共享实例。历史名称还渗入 adapter 类型名、构造函数、`manager.rs` 文件名及当前架构文档，形成一串互相强化但不准确的称呼。

## 决定

1. 将公共 Rust 类型 `ToolsManager` 改名为 `ToolsFacade`，不保留兼容别名。
2. 将其实现模块 `manager.rs` 改为 `facade.rs`；将 `from_tools_manager`、`agent_tool_ports_from_manager` 与 `ToolsManager*Adapter` 分别改为 facade 术语。
3. 将 `OperationCatalog` 中指向该 façade 的字段命名为 `facade`，使持有者名称与角色一致。
4. 保持 ToolsFacade 实例数量、构造时序、服务共享方式、Agent ports、执行与授权调用路径不变。资源生命周期仍留在 `McpManager`、`SkillsEngine`、`ToolRunService` 等 owner。

## 替代方案

- 保留 `Manager`：拒绝。该类型不管理 MCP/Skills/ToolRun 等资源的生命周期，而是组合多个 owner 的稳定调用面。
- 改称 `Service`：拒绝。它不是一个单领域规则 owner，而是跨多个 Tools 能力的组合 façade。
- 迁移后再保留旧类型别名：拒绝。项目允许内部测试版本破坏性重构；双名称会留下角色漂移。

## 影响与验证

- 更新 `haven-tools` 导出、Agent/App 调用点、对应 adapter/构造函数名称、测试名称及架构/路线图文档。
- 这是跨 crate Rust API 重命名，不改变 Tauri command/event、前端合同、配置、provider/MCP wire、数据库或运行行为；不需要数据重置。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`、`scripts/check-adr-index.ps1`（519 条唯一编号记录、链接解析通过）与 `git diff --check`。workspace 测试全部通过；标记为手动性能 profile 的测试保持 ignored。

## 回滚

恢复类型、模块、adapter 与构造入口的旧名称，并同步恢复当前架构/路线图文档。本变更无持久化、IPC 或用户数据回滚。
