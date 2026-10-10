# 0903：将 MCP 与 Skills 具体 owner 留在 Tools 内

## 状态

已接受并实现（2026-10-11）。

## 背景

`ToolServices` 已把授权、ToolRun、live output、受管资产和 Skill execution 收敛为 capability ports，但仍把具体 `McpManager` 与 `SkillRegistry` 交给 App/Agent。App 直接调用 manager 的连接状态、snapshot、reconcile、事件订阅和关闭方法；Settings 直接启动监视器。App/Agent 也直接访问 registry 的技能配置、重扫、列表、查询和版本。具体类型即使不公开其内部 map，也让消费者绑定到 Tools 内部 owner 实现，并由调用方自行组合 MCP refresh、技能刷新及 catalog 更新步骤。

这与 ToolRun、授权等已有消费端 port 边界不一致，也使 `McpManager`、`SkillRegistry` 的替换或职责调整必须同步修改 App/Agent。

## 决定

- `ToolServices` 不再暴露 `McpManager` 或 `SkillRegistry`，改为 `Arc<dyn McpRuntimePort>` 与 `Arc<dyn SkillCatalogPort>`。
- MCP runtime port 只提供 App 需要的监视器启动、shutdown、状态快照、client presence/status、refresh plan 和状态事件订阅。refresh diff 转换由 Tools owner 执行，不再向 App 暴露 `McpReconcile`。
- `McpStatusEventReceiver` 隐藏 Tokio broadcast channel；订阅者收到有序状态 DTO，lagged receiver 在 owner adapter 内继续读取，channel 关闭时返回 `None`。
- LLM 请求继续经 `haven_llm::McpToolCaller` trait object 调用 MCP 工具；它表达 provider caller 能力，不暴露连接管理器。
- `SkillCatalogPort` 只暴露 catalog version、配置应用、磁盘刷新、列表/单项查询和解析后的 skills root。实际目录状态仍由 Tools 内的 `SkillRegistry` 拥有。
- 普通生产 `haven-tools` API 不再重导出 `McpManager` 或 `SkillRegistry`。MCP manager 仅在显式 `test-support` 构建可见，供现有测试 adapter fixture 使用。

## 影响

- App 命令、Settings、bootstrap 与 Agent prompt context 保持原有 status、refresh、事件、技能发现和执行行为；调用依赖从具体 owner 类型改为命名能力 port。
- MCP 连接及 catalog revision 仍归 `McpManager`，技能发现、版本和 allowlist 仍归 `SkillRegistry`；此次只收紧跨层依赖方向，不合并两个 owner。
- Tauri IPC、事件 payload、配置、数据库、provider 请求与安全授权契约不变。Rust API 有意破坏性收口，不保留兼容入口。

## 验证

`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check` 与 `corepack pnpm run test:run` 均通过。UI 测试 1,029 项通过；Rust workspace 测试通过，手动性能测试按项目约定忽略。

## 替代方案

- 继续把 manager/registry 作为稳定 domain handle 保留：拒绝，因为真实消费者只需要状态/目录和少量具名操作，而宽 owner handle 仍允许调用方绑定生命周期实现。
- 为 MCP 和 Skills 建一个通用 `ToolServicesPort`：拒绝，因为会重新组合不同 owner 的权限，窄消费端 ports 更能约束调用范围。
