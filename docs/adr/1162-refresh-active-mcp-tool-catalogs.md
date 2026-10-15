# ADR 1162：在会话目录快照前刷新已激活的 MCP 工具

## 状态

已采纳

## 背景

MCP `notifications/tools/list_changed` 会更新 manager 的 tools/list 快照并提升 MCP catalog generation，但 `McpToolAdapter` 保存的是构造时的工具信息。已有 session overlay 替换同名 adapter 时只为新增名称提升 session version；Agent 的 provider tool-definition 缓存又只使用全局目录与 session overlay 版本。因此服务端改动参数或删除工具后，目录快照和模型请求可能继续使用旧 adapter。

## 决定

1. 构造每轮 `ToolCatalogSnapshot` 前，按 session 检查当前已激活的 MCP adapters，并读取对应 server 的最新 manager tools/list 缓存。更新已有工具的 schema、描述和由 adapter contract 导出的策略；manager 确认工具已删除或 server 已卸载时，从 session overlay 移除对应 adapter。
2. 同步只遍历当前已激活的 adapter。服务端新增的工具继续留在 dormant catalog 中，只有显式 `load_mcp` 会将它们加入 session，并继续受既有工具预算约束。
3. 同名工具的可见 manifest 变化（含 schema、描述和 policy）提升 session overlay version；相同 manifest 的重复加载不改变该版本。删除已激活工具也提升版本。
4. `ToolCatalogVersion` 同时包含 global catalog、session overlay 和 MCP manager generation。Agent 用完整版本决定是否复用 provider tool definitions；刷新失败且 manager 仍持有 client 时，保留最近可用 adapter，并在后续快照重试。
5. 不在此切片引入 `outputSchema`、`structuredContent` 或 MCP `input_required` 交互恢复语义；这些需要独立协议和生命周期决定。

## 替代方案

- 只将 MCP generation 加入 Agent 缓存键：拒绝，因为它只会重建缓存，仍会从旧 adapter 生成 schema。
- 要求每个 session 再次调用 `load_mcp`：拒绝，因为通知后的刷新应作用于已激活工具，不能依赖模型额外操作。
- 自动激活服务端新增工具：拒绝，因为这会改变现有按需加载与请求预算行为。

## 影响

不改变数据库、配置或持久会话格式，无需重置。会话中已删除或卸载 server 的 MCP 工具会在下一次目录快照中消失；暂时无法读取列表时保留已有 adapter，避免把短暂刷新失败误判为删除。

## 验证

- 定向回归覆盖 MCP 通知刷新失败后重试、Tools 已激活工具的 schema/描述刷新和删除、session overlay 同名重复加载版本，以及 Agent MCP generation cache miss。
- `cargo test --locked -p haven-tools --lib`：947 passed，2 ignored；`cargo test --locked -p haven-agent`：669 passed，1 ignored。
- 设置 `CARGO_PROFILE_TEST_DEBUG=0` 后，`cargo test --workspace --locked` 全部通过；`cargo clippy --workspace --locked -- -D warnings`、`cargo fmt --all -- --check` 也通过。

## 回滚

恢复之前的目录快照与缓存版本逻辑即可；本决定不涉及持久数据迁移或重置。
