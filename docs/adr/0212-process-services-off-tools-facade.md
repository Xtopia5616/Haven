# ADR 0212：进程服务移出 ToolsManager facade

- 状态：accepted
- 日期：2026-09-23
- 范围：`haven-tools`、`haven-agent`、`haven-app-binary`
- 关联：完成 [ADR 0211](0211-operation-registry-and-platform-snapshot.md) 留下的 service locator 调用点。不新增第二套 `AppRuntime`。

## 背景

ADR 0211 把执行和目录查询转到 `AuthorizedExecutor` / `OperationCatalog`，并把模型与媒体客户端收成 `PlatformRuntime`。`ToolsManager` 仍向 `haven-agent` 和 `haven-app-binary` 暴露 `mcp_manager()`、`skills_engine()`、`action_service()` 等 getter，调用方把 facade 当成 service locator。文档把 `OperationSpec` 写成全部 operation 的唯一定义，也把 `ToolsManager` 写成纯 facade；这两句都超过了代码。

## 决定

1. 进程服务在 `ToolsManager` 构造时收成 `ToolServices`：MCP、MCP 配置、skills、skill runner、授权、媒体资产、action、live output。`share_services()` 只交出这一份 bundle。组合根 `ApplicationRuntime.services` 持有它；session supervisor 与 app 命令使用同一份，不再向 manager 逐个取服务。
2. 不另建 `AppRuntime`。`ApplicationRuntime` 继续是进程组合根。
3. `OperationSpec` 只描述 builtin operation view，没有 handler。聚合工具在注册时把策略拷进 spec。MCP 自己实现 `tool_manifest`，Skill 使用 `Tool::operation_policy` 的默认实现。`ToolPolicy`、`ToolManifest`、`ToolPresentation` 保留为 IPC 形状，不改 `get_tools` 的 wire。
4. `AuthorizedExecutor` 不做交互式确认。它只做熔断、启用检查、校验、执行和结果分类。确认仍由调用方在 `execute_tool` 之前决定。
5. messaging runtime 与 memory recall 放进 `StartupWiring`，由 `wire_startup` 绑定一次，不再由 `app_state` 事后 `bind_*`。它们是进程服务，不进入可替换的 `PlatformRuntime`。tool settings、context limits、shell 与 security 和模型/媒体客户端写在同一份快照里。`admin_surfaces` 随成功的 catalog rebuild 进入 `BuiltinCatalog`，重建前为空。热更新走 `set_router_and_media_clients`，不保留单独的 `set_tts_client`。

## 替代方案

- 继续在 `ToolsManager` 上保留 getter：调用点短，但 facade 仍是 service locator，故不采用。
- 删除 `ToolManifest` / `ToolPolicy` / `ToolPresentation`：会改变 IPC。这些类型是投影形状，不是漏删的转换函数。
- 把交互式确认放进 `AuthorizedExecutor`：暂停确认和工具 future 会缠在一起。ADR 0211 已明确留下。
- 再引入 `AppRuntime { config, model, operation, memory, job, platform }`：组合根已经是 `ApplicationRuntime`。

## 影响

- 工具名、权限 key、provider schema、数据库和 `get_tools` manifest 形状不变。
- `ToolsManager` 仍转发执行与目录查询，并保留启动装配和录音转写。它不再发放进程服务。
- 不改变持久化 schema 或 IPC 字段名。

## 验证

- `cargo fmt -p haven-tools -p haven-agent -p haven-app-binary`
- `cargo clippy --locked -p haven-tools -p haven-agent -p haven-app-binary -- -D warnings`
- `cargo test --locked -p haven-tools --lib`
- `cargo test --locked -p haven-agent`

## 回滚与重置

不改变持久化或配置 schema。回滚代码即可，无需用户数据重置。回滚时必须同时恢复 `ToolServices` 调用点和 `StartupWiring` 里的 messaging / memory 字段。
