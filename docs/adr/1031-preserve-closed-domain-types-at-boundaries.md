# 1031：跨边界保留闭合领域类型

## 状态

已采纳并实施（2026-10-12）。

## 背景

ToolRun 取消命令接收 App-owned `ToolRunKindDto`，随后将它降为字符串并经 App/Tools capability 传递，最终由 Tools 再解析为 `background` 或 `scheduled`。同一命令的 history 查询已经在 App 边界映射为 `ToolRunKind`，两条调用链的类型约束不一致。

WebSearch phase 在 LLM provider 侧已有闭合 `WebSearchPhase`，但 Agent event 和 App IPC DTO 将其降为 `String`，UI mapper 也接受任意字符串；测试中的 `started` 不属于生产 phase 集合。

## 决定

- App 在命令边界显式将 `ToolRunKindDto` 映射为 Tools-owned `ToolRunKind`，之后通过 App port、Tools capability 和 `ToolRunService` 保留闭合类型。持久化字符串仍由 Tools owner 编解码。
- App 集中维护 `ToolRunKindDto` 与运行时 `ToolRunKind` 的双向显式映射，避免多个 event/command mapper 重复写匹配表。
- Agent event、App event DTO 和生成的 UI IPC contract 均保留 `WebSearchPhase`。UI mapper 按生成的 `in_progress`、`searching`、`completed` 值集校验，拒绝未知值。
- 定时 ToolRun 生命周期与恢复调用链使用 `tool_run_id`、`watched_tool_run_id` 等完整实体名称。

## 影响

ToolRun kind mismatch 仍返回 `false`，但字符串重解析和不可能的无效 kind 输入已从内部调用链移除。WebSearch 的 JSON 字符串值不变，UI 的静态 contract 收窄为生产闭合值集；completed 事件仍投影为已结束的工具消息。数据库 schema、持久 event payload 和用户可见行为不变。

## 验证

- `cargo test --locked -p haven-tools test_tool_run_kind_and_terminal_delete_guards`
- `cargo test --locked -p haven-tools scheduled`
- `cargo test --locked -p haven-agent web_search_overflow_pump_preserves_order_when_channel_is_full`
- `cargo test --locked -p haven-app-binary channel_maps_every_variant_to_expected_channel`
- UI WebSearch contract/reducer focused tests、`corepack pnpm run check`
- IPC contract generator 与 `git diff --check`

## 回滚

回滚时仍需保持 App 与 Tools 的 `ToolRunKindDto` / `ToolRunKind` 所有权分离，且在 App 边界完成映射；不得重新把内部能力参数放宽为字符串。WebSearch phase 若改回开放文本，必须同步说明新增 provider phase 的 UI fallback 语义。
