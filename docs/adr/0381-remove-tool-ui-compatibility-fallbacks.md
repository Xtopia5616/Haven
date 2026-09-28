# ADR 0381：删除工具结果渲染与 MCP 状态兼容分支

- 状态：已采纳（2026-09-27）
- 范围：MCP catalog status contract、Agent Observation renderer contract 与工具结果卡片分类
- 关联：ADR 0347、0369、0380

## 背景

Rust 的 `McpClientStatus` 是固定的 serde 外部标记 enum，但 UI 曾将任意字符串和任意外部标记对象视作有效状态，并在 MCP server card 中展示未知变体。ADR 0369 当时为该开放式 status contract 保留了未来变体透传。

Rust Agent Observation 始终携带必填 `renderer`，它由当前 operation manifest 提供并存入提交事件。前端事件 contract 却允许省略该字段；缺少 renderer 时，`toolResultParsing.ts` 又通过 inspection payload shape 猜测专用卡片类型，以服务旧事件或旧 transcript。

当前版本仍处于测试阶段，不继续承担这些尚未形成稳定契约的旧分支。

## 决定

1. `McpClientStatus` 精确表达 Rust 的 `Disconnected`、`Connecting`、`Connected` 与 `{ Offline: { error } }`。`listMcpTools` 在 typed helper 边界校验 status；未知或结构错误的 variant 拒绝整份响应。MCP server card 只显示该固定集合。动态 schema 和 snapshot 上的其他附加字段仍按 ADR 0369 原样透传。
2. Agent Action/Observation 的前端 DTO 与 mapper 按 Rust DTO 校验必需字段：`tool_call_id` 必须显式为字符串或 null，Observation 的 `renderer` 和结果 envelope 必填。`mapAgentEvent` 对缺失或结构错误的字段拒绝事件；IPC drift script 固定 renderer 的 Rust/TypeScript 必需性。
3. 结构化工具结果只有在 Observation renderer 或当前 manifest 明确给出 renderer 时才进入专用结果路径。删除按 payload shape 猜测 renderer 的 `customShape` 分支。事件缺少 renderer 且工具名无法由当前 manifest 解析时，历史 JSON 结果显示为通用 JSON 卡片；shell、notify、原始文本与空内容的既有特殊行为保持。
4. `toolResultRenderers.ts` 中 operation family 内部对当前结果字段的路由继续保留，例如 `system` renderer 按 scope 选进程/窗口子卡片；这些路由服务当前 manifest 的共享 renderer，不再负责猜测缺失的 renderer contract。

## 替代方案

保留开放 MCP status 与 payload shape 推断 renderer，可以让 UI 暂时容忍未来变体和缺字段历史事件；但目前 Rust status 是固定 enum，当前 Agent 事件也已提供 renderer，保留这些分支只会让 UI 契约比生产者更宽并维持过时路径，因此不采用。

## 影响与重置

未知 MCP status 将不再作为可接受的 UI contract。缺少 Observation renderer 的事件会被 mapper 丢弃；缺少该字段且无法由当前 manifest 解析工具名的旧结构化结果仍可读，但显示为通用 JSON 卡片。动态结果内容、snapshot 附加字段、数据库和持久化 event payload 均未修改，无数据库或配置重置步骤。

## 验证

```sh
corepack pnpm run check
corepack pnpm run test:run
corepack pnpm run build
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
pwsh -NoProfile -File scripts/check-ipc-events.ps1
git diff --check
```

验证结果：全部通过。`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI `check` / `test:run` / `build`、Prettier 检查、两个 IPC 契约脚本与 `git diff --check` 均通过。

## 回滚

回滚本 ADR 对应提交可恢复开放式 MCP status contract、可选 Observation renderer 与基于结果形状的旧 renderer 推断；无数据或 schema 回滚步骤。
