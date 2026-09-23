> 2026-09-23：进程服务已从 facade getter 迁到 ToolServices。OperationSpec 不是全部 operation 的定义。见 [ADR 0212](0212-process-services-off-tools-facade.md)。本记录的执行入口、PlatformRuntime 与不新增 AppRuntime 仍然有效。

# ADR 0211：ToolsManager 收成执行入口，operation 单一投影

- 状态：accepted
- 日期：2026-09-23
- 范围：`haven-tools`、`haven-common` 工具目录 DTO
- 关联：收紧 [ADR 0142](0142-tool-manifest-operation-policy.md) 的双策略来源，并替代 [ADR 0162](0162-tool-manager-boundaries.md) 里逐字段绑定平台客户端的方式。0162 的 core / runtime / builtins 方向仍然有效。

## 背景

`ToolsManager` 在 ADR 0162 / 0205 之后仍同时编排注册、session catalog、deferred catalog、MCP/Skill 装载、授权、媒体资产、action、router 和音频客户端。`OperationPolicy` 与 `ToolPolicy`、`ToolManifest`、`ToolPresentation` 互相转换，模型目录和 UI 可能各看一份策略。`ToolRuntime` 用多个 `RwLock<Option<Arc<_>>>` 先创建空槽再逐个 `bind`，热更新时一次设置变更会先后写入 router、STT、OCR、TTS，catalog rebuild 可能看到混代客户端。

## 决定

1. 模型可见 operation 的唯一定义是 `OperationSpec`（名称、schema、presentation、typed policy）。handler 与 spec 一起注册为 `OperationViewTool`。`ToolManifest`、`ToolPolicy`、`ToolPresentation` 只作为 Tauri/UI 投影，由 `project_tool_manifest` 统一生成，不再提供 `to_catalog_policy` 之类的反向或平行转换。
2. `OperationRegistry` 持有已安装 builtin、deferred catalog 和 session overlay。`OperationCatalog` 只读这组注册表，产出 provider definitions、UI manifests 和 turn snapshot。装载与准入仍由 facade 触发，但不再各自拼 manifest。
3. `AuthorizedExecutor` 是执行入口：准入、校验、运行、按工具声明的 metadata 分类结果。`ToolsManager` 上的执行与授权请求方法只转发到这里。交互式确认仍由调用方在执行前决定，避免在工具 future 内阻塞。
4. 模型、媒体和 admin 输入收成不可变 `PlatformRuntime`。启动与热更新整体替换这份快照；action、资产、messaging 和 memory port 仍是进程服务，不放进可空 bind 槽。`ApplicationRuntime` 继续是进程组合根，不在 tools 内再做一套 config/memory/job 容器。

## 替代方案

- 删除 `ToolManifest` / `ToolPolicy` 类型并把 UI 改成直接消费 typed policy：会改变 `get_tools` 的 wire 形状。投影类型保留，权威来源收成 spec。
- 把授权确认搬进 `execute_tool`：调用方目前必须先拿到 receipt 再执行，搬进去会把暂停确认和工具重试缠在一起。
- 在 `haven-app-binary` 再拆一套 `AppRuntime { config, model, operation, memory, job, platform }`：组合根已经是 `ApplicationRuntime`。本次只去掉 tools 内部的字段级 bind。

## 影响

- 工具名、权限 key、provider schema、数据库和 `get_tools` manifest 形状不变。
- 热更新读者持有替换前的 `Arc<PlatformRuntime>`，不会看到半更新的客户端组合。
- 不改变持久化 schema 或 IPC 字段名。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-tools -p haven-agent -p haven-app-binary`
- `cargo clippy --locked -p haven-tools -- -D warnings`
- `cargo test --locked -p haven-tools --lib`

## 回滚与重置

不改变持久化或配置 schema。回滚代码即可，无需用户数据重置。回滚时必须同时恢复 `PlatformRuntime` 替换、`OperationRegistry` 字段和 manifest 投影入口。
