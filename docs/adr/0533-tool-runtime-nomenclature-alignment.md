# ADR 0533：工具运行时命名对齐职责

## 状态

已采纳并实施；Rust workspace 门禁通过（2026-10-06）。

## 背景

工具运行时的几个名称把不同职责描述成了同一类“目录”或“策略”：`SessionCatalog` 实际保存特定 session 当前可执行的附加工具；`ToolAuthorizationPolicy` 不做授权裁决，而是解析操作契约并构造授权请求；`get_*` 方法生成派生策略/请求，`list_defs` 则使用了不透明缩写。调用者需要阅读实现才能知道这些名称的真实边界。

同时，`ToolRegistry`、`DeferredToolCatalog` 和 session 工具集合虽然有相似的查找/列举外形，却分别管理已安装注册、尚未激活的发现项和 session 执行作用域。它们的准入、版本、排序与生命周期不同，不应为了名称整齐而合并。

## 决定

1. 将 `SessionCatalog` 及其成员/端口命名改为 `SessionToolOverlay`，明确它是全局 `ToolRegistry` 之上的 session-scoped 可执行工具集合。
2. 将 `ToolAuthorizationPolicy` 改为 `ToolAuthorizationRequestResolver`。它只解析 operation policy、risk 与 `AuthorizationRequest`；允许、拒绝或确认仍由 `AuthorizationEngine` 决定。
3. 将负责派生解析的方法 `get_risk_level`、`get_operation_policy`、`get_authorization_request*` 改为 `resolve_*`；将 `list_defs`、`list_enabled_builtin_defs` 与 `select_tool_defs_for_budget` 改为完整的 tool-definition 名称。Agent prompt context 的 `builtin_defs` 字段也改为 `builtin_tool_definitions`。
4. 保持 `ToolRegistry`、`DeferredToolCatalog`、`SessionToolOverlay` 三个集合 owner 分离。`ToolCatalogSnapshot` 继续提供 turn 内共享的不可变视图。
5. 删除旧 Rust 符号，不添加兼容别名。当前项目允许内部测试版本做破坏性重构；仓库内调用点随本切片一起更新。

## 替代方案

- 将三个集合合并为统一 Catalog/Registry：拒绝。其状态作用域、准入、版本与生命周期不同，合并会增加配置分支并模糊 owner。
- 保留 `Policy` 和 `get_*` 名称：拒绝。它们把“解析请求”和“作出授权决定”混为一谈，也弱化了派生值的行为语义。
- 仅新增更清楚的别名、继续保留旧名称：拒绝。双名称会延续本轮要消除的词汇漂移。

## 影响与验证

- 更新 Rust workspace 中对应的类型、字段、trait/adapter、方法调用和测试名称，并更新当前架构图与路线图。
- 不改变工具注册、按需加载、授权判断、执行顺序、版本时钟或错误行为；不改变 Tauri IPC、事件、数据库 schema、配置、provider/MCP wire 或用户可见文案。因此无需数据重置。
- 本 ADR 是全项目术语与架构角色审计的第一个 Tools 切片，不代表 Tools 或全仓审计完成。路线图 §5.7 继续保持 Active。
- 验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked -- --test-threads=1`。测试使用执行前不存在的隔离 `APPDATA` 根目录 `target/audit-runtime-data-0533/AppData/Roaming`；手工性能 profile 按项目约定忽略。工作区 check 曾等待另一个 Cargo 开发构建释放共享 target 锁，之后完整通过。

## 回滚

如需回滚，仅恢复本 ADR 列出的旧 Rust 符号及其调用点，并同步恢复架构文档与路线图中的旧名称。无 schema、IPC 或持久数据回滚。
