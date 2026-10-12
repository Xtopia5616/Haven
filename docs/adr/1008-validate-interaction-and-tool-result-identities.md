# 1008：校验交互 replay 与 ToolRun 结果身份

## 状态

已采纳并实施（2026-10-12）。

## 背景

Interaction replay 过去只核对 session 关联，不核对 durable payload 内的 `request_id`、Ask/Confirm step、清理列表或 ScheduledConfirm 的 ToolRun 身份。builtin `tool_runs`、`schedule` 与 `shell` 结果 renderer 也把 `tool_run_id` 当任意字符串。两处边界会让错误实体类型或畸形身份进入恢复状态及专用 renderer。

## 决定

- Session interaction replay 按实体校验 `ses-*`、Ask `step-*`、Confirm `conf-*`、Confirm step `step-*` 和 ScheduledConfirm `toolrun-*`，并校验 Ask request 与首个 step 的身份关系。
- interaction clear payload 只接受 canonical `step-*` 或 `conf-*` request IDs；错误类型、格式或关联关系使 replay 失败。
- UI builtin `tool_runs`、`schedule` 和 `shell` 结果对可选 `tool_run_id` 及数组行统一要求 canonical `toolrun-*`；格式错误时退回通用 JSON renderer。

## 影响

不改变 durable event 或 tool-result payload 的字段形状，也不迁移数据库。格式正确的身份保持原值；畸形 durable interaction 不再恢复为 pending 请求，畸形工具结果不再进入专用 renderer。

## 替代方案

- 只校验字符串：拒绝；无法区分 session、step、confirmation 与 ToolRun 身份。
- 在 UI 修补时重写或猜测 ID：拒绝；实体身份由写入 owner 生成，renderer/replay 只能验证。

## 验证

- `cargo test --locked -p haven-agent interaction::tests`
- `cargo test --locked -p haven-agent --lib session::tests::interaction_events_persist_and_replay_into_reloaded_actor`
- `corepack pnpm exec vitest run src/lib/ToolResultCard.test.ts`
- `cargo clippy --workspace --locked -- -D warnings`
- UI 类型检查在本轮全量验收中复跑。

## 回滚

可分别撤回 replay 和 renderer 的 runtime ID guards；撤回会重新允许非 canonical identity 通过相应边界，不涉及持久化回滚。
