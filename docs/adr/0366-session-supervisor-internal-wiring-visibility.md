# ADR 0366：收窄 SessionSupervisor 内部 wiring 可见性

- 状态：已采纳（2026-09-26）
- 范围：`SessionSupervisor::get_tools()` 与 `SessionSupervisor::services()` 的 Rust 可见性
- 基线：HEAD `0708a9b`；开始时工作区干净
- 关联：[ADR 0363](0363-session-supervisor-typed-store-constructor.md)、[ADR 0257](0257-explicit-tool-catalog-injection.md)

## 背景与调用审计

`SessionSupervisor::get_tools()` 和 `SessionSupervisor::services()` 返回由 supervisor 持有的工具管理器与工具服务。全仓 Rust 源码搜索确认两者的调用点均位于 `haven-agent` crate 内，包括 crate 测试；`app-binary` 与 workspace 其他 crate 没有调用。`app-binary::runtime::ApplicationRuntime::services()` 是不同类型上的同名方法，不属于本次 API。

`SessionSupervisor` 本身继续作为 agent crate 的公开类型导出，但这两个访问器只服务 agent 内部 wiring 和执行路径，不应构成跨 crate service locator。仓库搜索不能排除未知的外部 Rust 下游调用者。

## 决定

1. 将 `get_tools()` 和 `services()` 从 `pub` 收窄为 `pub(crate)`，并在方法处说明它们仅供 agent 内部 wiring 使用。
2. 不改变 `ToolServices` 内容、`ToolsManager` ownership、`SessionActor`、`AgentLayer`、runtime behavior、DB/X12、IPC 或 UI。
3. 不在本切片重构 agent crate 内的调用链；全局完成定义中关于内部 facade/service locator 的待办仍按域审计。

## 替代方案

- 保持两个方法公开会继续暴露无 workspace 外部调用者的实现访问入口。
- 将工具与服务显式注入所有 agent 内部消费者属于更大范围的 ownership/wiring 重构，超出本次纯可见性收窄。

## 影响、兼容性与回滚

这是 Rust source API 收窄，没有持久数据、配置、wire 或数据库 schema 变化，也没有数据重置要求。未知外部调用方需要改用受支持的 agent crate API；不保留 public 兼容 wrapper。回滚时恢复方法为 `pub` 即可，但会重新暴露这两个 service locator 入口。

## 验证

按本轮任务要求运行：

```text
cargo fmt --all -- --check
cargo test --workspace --locked
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
```

四项 Rust 门禁均通过。Workspace 测试全部成功；Windows linker 输出了现有的 `linker_messages` 提示，没有测试失败。
