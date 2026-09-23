# ADR 0233：会话托管媒体资产租约端口

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 会话恢复、运行和清理路径
- 关联：[ADR 0211](0211-operation-registry-and-platform-snapshot.md)、[ADR 0212](0212-process-services-off-tools-facade.md)、[ADR 0224](0224-tool-catalog-port.md)

## 背景

会话恢复和结束清理需要为消息附件注册、释放 session-scoped managed asset lease。此前这些路径通过
`get_tools()` 取得完整 `ToolsManager`，再直接调用资产注册表操作，导致会话生命周期代码知道执行总管的具体类型，
也让资产租约成为继工具目录、观察文本之外的第三条宽 facade 依赖。

工具 session overlay 的 `unregister_session` 是另一种恢复语义，不能与媒体资产租约合并；它继续由恢复工具路径单独管理。

## 决定

1. `SessionSupervisor` 持有 `ManagedAssetLeasePort`，只暴露按 session 注册和释放附件资产租约的两个操作。
2. 生产适配器仍委托 `ToolsManager` 的资产注册表，因此资产路径校验、引用计数和释放规则仍只有一处实现。
3. `resume.rs` 和 session status 清理路径只通过 supervisor port 操作资产租约；不再为此调用 `get_tools()`。
4. 不改变工具执行、授权确认、MCP/Skill session overlay 恢复或资产 ID/wire 契约。

## 替代方案

- 继续在调用点使用 `get_tools()`：实现简单，但会继续扩大 Agent 对 ToolsManager 的结构依赖，拒绝。
- 把资产注册表复制到 Agent：会产生第二个租约事实和释放规则，拒绝。
- 把 `unregister_session` 与资产释放合并：两个生命周期不同，可能在恢复中错误清除工具 overlay，拒绝。

## 影响与验证

会话层只依赖资产租约意图；`ToolsManager` 仍是唯一资产注册表 owner。覆盖媒体恢复、租约隔离和 agent 编译约束的测试保持通过。

验证：`cargo fmt --all -- --check`、媒体恢复定向测试、`cargo test --locked -p haven-agent --lib`、
`cargo clippy --locked -p haven-agent --lib -- -D warnings`。

## 回滚

恢复 `resume.rs` 与 session status 中的直接 `ToolsManager` 调用，并移除 `ManagedAssetLeasePort` 及其适配器；不涉及数据库、schema 或用户数据。
