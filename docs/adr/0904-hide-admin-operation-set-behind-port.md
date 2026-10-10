# 0904：通过 AdminOperationPort 隔离 AdminOperationSet

## 状态

已接受并实现（2026-10-11）。

## 背景

`ToolsFacade::admin_surfaces` 将具体 `AdminSurfaces` 对象交给 App。App 随后直接调用它的 `metadata` 与 `execute`，而模型侧的五个 typed adapters 也从同一对象构造。该对象不是可独立共享的领域数据，而是 Tools catalog generation 内部的 operation set；将其具体类型暴露给 App 会让调用方依赖 Tools 的装配实现。

这一出口与刚收紧的 MCP、Skills owner 边界相同：跨层需要的是具名能力，而不是 owner 实现对象。

## 决定

- 内部类型改名为 `AdminOperationSet`，只由 Tools 的 builtin catalog 持有，并随成功的 catalog rebuild 一起发布。
- `AdminOperationSet` 不再通过 `builtin` 或 `haven-tools` crate root 重导出；实现结构及其构造方法仅在 Tools crate 内可见。
- `ToolsFacade::admin_operations` 返回 `Option<Arc<dyn AdminOperationPort>>`。该 port 只提供 typed request 的授权风险级别查询与执行；App 不取得其余 operation metadata、catalog、typed adapters 或共享 `AdminServices`。
- App command helper 命名为 `execute_admin_operation`，保留既有授权、队列确认和执行顺序。
- 模型侧仍由 `AdminOperationSet` 构造五个 typed adapters；native App 请求与模型工具继续共用同一 operation 实现和输出契约。

## 影响

- App 只能通过 Tools 所属的 `AdminOperationPort` 执行 typed admin 请求，具体组合与 operation 生命周期归 Tools。
- `AdminRequest`、`ToolOperationMetadata`、`ToolResult` 与 Tauri IPC/event、配置、持久化及安全策略不变。
- Rust API 破坏性收口；不保留 `admin_surfaces` 或 `AdminSurfaces` 兼容入口。

## 验证

`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings` 均通过。App 235 项测试另以 8 个测试线程连续运行三次，均通过；ADR 索引校验通过。

## 替代方案

- 保留 public `AdminSurfaces` 并仅隐藏字段：拒绝，因为 App 仍会直接绑定实现对象的方法集合与生命周期。
- 让 App 自行拼出 metadata 和 operation dispatch：拒绝，因为风险 metadata 与执行实现必须由同一 owner 提供，避免两份映射漂移。
