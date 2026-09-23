# ADR 0244：类型化运行时 Web Search 能力

- 状态：已采纳（2026-09-24）
- 范围：`ToolsManager::runtime_capabilities` 与 Agent system prompt runtime snapshot
- 关联：[ADR 0148](0148-three-layer-capability-discovery.md)、[ADR 0164](0164-model-experience-p2-polish.md)、[ADR 0224](0224-tool-catalog-port.md)

## 背景

`RuntimeCapabilities.web_search` 原来用 `String` 同时表达运行时能力来源和 prompt 展示文字。Tools 已负责判断 provider 与 MCP 能力，Agent prompt 却隐式依赖这些字符串值；拼写或文案调整可能改变业务状态表达，类型系统也无法约束三种有效状态。

`RuntimeCapabilities` 是跨 crate 的公开 Rust API，但没有 serde 派生、持久化用途或 IPC 消费者。当前唯一生产消费者是 Agent 的 runtime prompt snapshot。prompt 中既有三个值为 `provider`、`mcp` 和 `unavailable (no provider builtin search; no MCP search server)`。

## 决定与事实所有权

1. 在 `haven-tools` 定义 `WebSearchAvailability::{Provider, Mcp, Unavailable}`，并将 `RuntimeCapabilities.web_search` 改为该枚举。Tools 是能力事实与来源选择的 owner。
2. `ToolsManager::runtime_capabilities` 继续判定 MCP catalog 中是否存在搜索工具；provider 仅在已配置 Chat route、web-search mode 不是 `Off` 且当前 adapter 支持内置搜索时成立。provider 与 MCP 同时成立时仍优先选择 Provider。
3. Agent prompt 层将枚举映射为现有 prompt 字符串。提示词文案属于 Agent 的呈现职责；Tools 不再为 prompt 格式化能力状态，路由探测不移入 Agent。
4. 枚举只作为 Rust 运行时 API 使用，不增加 serde 行为，不修改 IPC、数据库、配置或 provider wire contract。由 `String` 改为枚举是有意的公开 Rust 源码契约变更，仓库内消费者同步迁移。

## 必须保持的不变量

- provider search 只由已配置的 Chat route 决定；不能探测默认 endpoint 来冒充已配置 route。
- provider 判定继续同时要求 route 存在、mode 非 `Off`、adapter 支持 builtin web search。
- provider 与 MCP 同时可用时选择 provider；provider 不可用时才回退到 MCP；两者均不可用时返回 Unavailable。
- MCP 判断继续基于启用服务器的已发现工具名；仅服务器名称包含 search 字样不构成能力。
- Agent 输出的三个 prompt 值逐字保持 `provider`、`mcp`、`unavailable (no provider builtin search; no MCP search server)`。
- vision、image generation、transcription、recording、TTS 的判断及 runtime snapshot 其他字段不变。

## 明确不做

本切片不重写或拆分 `ToolsManager`，不统一 provider 与 MCP catalog，不调整 capability probing、adapter 能力表、缓存时钟、安全策略或 prompt runtime snapshot 的其他字段，也不引入通用状态序列化或兼容字符串转换层。

## 替代方案与影响

保留 String 会继续让跨 crate 状态契约依赖自由文本；给枚举实现 `Display` 会把展示格式放回 Tools，继续混合事实与呈现。故采用公开类型表达状态，由 Agent 显式映射 prompt 文案。

该 API 调整要求仓库外 Rust 调用者将 `String` 使用迁移到 `WebSearchAvailability`。数据库、配置文件、IPC 和 provider payload 不受影响，不需要 schema 或数据重置。

## 验证与回滚

单测覆盖 Provider/Mcp/Unavailable 选择、provider 优先级、无可用 provider/MCP 时的状态，以及三种 prompt 文案逐字映射。实施门禁为 Rust 格式检查、`haven-tools` 与 `haven-agent` 测试、workspace 严格 Clippy。

回滚时恢复 `RuntimeCapabilities.web_search: String`、Tools 的旧字符串构造和 Agent 的直接展示，并移除此 ADR 与索引/路线图记录；无需数据库或配置迁移。
