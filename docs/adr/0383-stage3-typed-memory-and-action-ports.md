# ADR 0383：阶段 3 的 typed memory 与 Action 投影验收

- 状态：已采纳并实现（2026-09-28）
- 关联：[ADR 0275](0275-action-status-list-typed-projections.md)、[ADR 0299](0299-react-compaction-summary-memory-store-port.md)、[ADR 0312](0312-memory-worker-summary-extraction-state-store.md)、[ADR 0361](0361-final-architecture-acceptance-audit.md)、[ADR 0364](0364-agent-layer-memory-service-constructor.md)、[ADR 0374](0374-typed-session-cleanup-and-explicit-agent-tool-wiring.md)

## 背景

MemoryWorker 与 ReActEngine 的生产持久化已通过 typed stores 收口，但 `MemoryService` 构造仍接收并保留 raw `Database`，再从中创建 stores。ActionService 也已有命名 status/list projections，不过部分跨 crate 调用仍经 JSON `Value`，稳定字段需在调用方重新解读。

## 决定

1. `AppState` 作为组合根创建 memory repositories，并通过 `MemoryServiceStores` 把 `MemoryStore`、fact/extraction/maintenance/recall stores 与 embedding store 注入 Agent。生产 `MemoryService` 不接收、不保存 raw `Database`；测试 fixture 的 Database 转换只在 `cfg(test)` 下存在。
2. `ActionService` 的 board、session list、status 与 scheduled-list 读取使用命名 typed projections。Agent 的 paused-session waiting-reason 判断只消费 `ActionListView.status/kind`，不解析 JSON 字段。
3. Action 与 scheduled tool 的 JSON 序列化留在 ActionsTool、ScheduledActionTool、shell/tool result 或 event wire 边界。MCP/Skill schema、动态 tool args/output、provider payload、通用工具 observation 仍是明确的动态 JSON 扩展点，不作为稳定业务 DTO 的默认形式。
4. App composition root 可以持有 `Database` 来创建 typed stores；`haven-agent` 与 `haven-tools` 的生产业务路径不持有 raw Database。ToolsManager execution/adapters 仍按 ADR 0374 作为阶段 4 边界跟踪，不把它算作阶段 3 的 Database 穿透。

## 影响与验证

此项只收窄 Rust 内部构造与 DTO 边界，不改 schema、IPC、模型可见工具 JSON、Action event payload 或数据库重置要求。memory store 事务/回滚/空结果回归继续使用内存 Database；typed Action 投影与工具 JSON 兼容测试覆盖字段和序列。

```text
cargo test --locked -p haven-agent
cargo test --locked -p haven-tools
cargo check --locked -p haven-app-binary
```

## 回滚

回退 typed-store 注入与 Action projection 调用即可；不需要数据库或配置重置。
