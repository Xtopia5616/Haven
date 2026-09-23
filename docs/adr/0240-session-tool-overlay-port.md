# ADR 0240：Session tool overlay 恢复端口

- 状态：已采纳（2026-09-24）
- 范围：`haven-agent` 的 MCP/Skill/builtin session overlay 恢复与清理
- 关联：[ADR 0224](0224-tool-catalog-port.md)、[ADR 0233](0233-session-managed-asset-lease-port.md)

## 背景

resume 和 session 清理路径直接通过 `ToolsManager` 执行 overlay 注销与恢复，使 Agent 了解工具总管的 session 注册 API。
资产租约、工具执行/授权和 live 注册具有不同生命周期，不能共用一个 port。

## 决定

1. `SessionSupervisor` 持有 `SessionToolOverlayPort`，封装 unregister、MCP、Skill 和 builtin overlay 操作。
2. resume 继续按事件投影 round 顺序串行、best-effort 恢复；MCP `None` 与 `Some([])` 保持不同语义。
3. 结束/删除注销时点、暂停保留行为和 live tool_runner 注册保持不变。
4. 不改变 catalog 预算、执行/授权、asset lease 或 ToolsManager 实现。

## 影响与验证

Agent 恢复/清理路径只依赖 overlay 意图；agent 468 项测试和严格 Clippy 通过。无 schema/IPC 变化。

## 回滚

恢复四类 overlay 的直接 ToolsManager 调用并删除该 port；不涉及持久数据。
