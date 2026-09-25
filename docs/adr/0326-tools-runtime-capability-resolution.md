# ADR 0326：Tools runtime capability resolution boundary

- 状态：Implemented
- 日期：2026-09-25
- 范围：`haven-tools` prompt-facing `RuntimeCapabilities` 解析及 `ToolsManager` facade
- 关联：[ADR 0257](0257-explicit-tool-catalog-injection.md)、[ADR 0306](0306-tools-admin-capabilities-through-typed-stores.md)

## 背景与不变量

`ToolsManager::runtime_capabilities` 同时取得 `PlatformRuntime`、解析媒体能力、构建/扫描 MCP index，并判断 provider built-in search。其调用链是 Agent prompt runtime snapshot → `ToolsManager::runtime_capabilities` → `ToolRuntime::platform` 与 `build_mcp_index` → prompt-facing capability DTO。`catalog.rs` 仍负责工具目录投影，不拥有这些能力策略。

本切片保持以下不变量：

1. Web search 按 provider、MCP、unavailable 的顺序选择。Provider 只有在 Chat route 已配置、搜索模式未关闭且 adapter 支持 built-in search 时可用；无 Chat route 时不以默认 endpoint 推断能力。
2. `vision` 与 `transcription` 继续来自现有媒体路由解析。Dedicated STT 可单独提供 transcription；`recording` 只取决于 capture pipeline，与 STT 分离。
3. Image generation 只取决于 image generation client，TTS 只取决于 TTS client。Prompt DTO 的字段映射与内置 media capability 保持一致。
4. MCP search 继续只从已构建 index 的缓存 tool names 识别；禁用服务器仍由 index 构建阶段排除。
5. 不改变 `PlatformRuntime` 字段、IPC、tool catalog wire、authorization 或 provider contract。

## 决定

1. 新增 crate-private `runtime_capabilities` 模块。它接收借用的 `PlatformRuntime` snapshot 和已构建 MCP index，集中解析 media/provider typed inputs，并通过纯映射函数产出现有 `RuntimeCapabilities` 与 `WebSearchAvailability`。不复制 snapshot，也不通过 service locator 查依赖。
2. `ToolsManager` 只负责取得当前 platform snapshot、构建 MCP index 并委托解析。Transcription ingress 与 recording transcription 的媒体能力 gate 复用同一模块中的 media resolver。
3. MCP index entry 的搜索识别逻辑移入新模块；已有 index 构建、安全过滤及模型可见 payload 保持不变。
4. `ToolsManager` 仍拥有 composition、runtime 更新及 catalog rebuild。此切片不引入 capability snapshot cache；完整 facade 解耦和 cache 的失效/版本语义仍待后续阶段处理。

## 替代方案

- 继续把 provider、media 与 MCP 策略留在 `ToolsManager`：保留能力策略与 composition/catalog facade 混合的现状，拒绝。
- 此时同时引入 capability cache 或将所有 facade职责搬出 `ToolsManager`：会增加 cache 失效语义或扩大跨域重构范围，超出本切片，拒绝。

## 影响与验证

- 无 schema、配置、IPC、provider tool contract 或授权变化，无数据重置要求。
- 新模块单测覆盖 provider 优先级、无 Chat route、无 STT 时仍可 recording、vision/image generation/transcription/TTS 映射与 MCP index search detection；现有 ToolsManager/MCP 集成测试保留。
- 验收命令：`cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`git diff --cached --check`。

## 回滚

删除 `runtime_capabilities` 模块及 ADR/路线图索引记录，并将 runtime capability、media resolver 和 MCP search detection 调用恢复到原位置即可。无持久化或 wire 数据需要迁移。
