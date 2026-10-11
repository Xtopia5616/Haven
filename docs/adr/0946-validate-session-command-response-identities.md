# 0946：在 Session command response 边界校验持久实体 ID

## 状态

已接受并实现（2026-10-11）。

## 背景

Tauri command DTO 生成了 TypeScript 静态类型，但 renderer 的 `invoke` 值没有运行时校验。Session history、runtime list、lineage 与 resume response 因此会把原始字符串 identity 直接送入 session switcher、resume merge 与 reducer。持久 ID 进入 UI 后参与跨行关联：message 与 step 可能共用 `step-*`，usage 行有独立 `usage-*`，interaction 的 ask identity 与步骤关联。

## 决定

- 在 `contracts/sessionHistory.ts` 统一解析 Session command response：session/lineage ID 使用 `ses`，runtime 与恢复步骤使用 `step`，恢复 message 使用 `msg` 或关联到步骤行的 `step`，usage 使用 `usage`。
- Resume 的 message、step、usage 必须属于返回的同一个 session；step-backed message 和 ask interaction 必须匹配 response 中实际返回的 step 行。Resume interaction 只接受由该 session 拥有的请求，ask 使用关联的 `step-*`，confirm 使用 `conf-*`。
- `list_runtime_sessions`、history、lineage、resume 与 latest-resume command adapter 都在返回领域对象前运行校验；非法响应以错误拒绝，latest-resume 的 `null` 仍表示无可恢复会话。
- Provider `tool_call_id` 是外部身份，保持原样，不要求 Haven 前缀。

## 影响

合法 Rust response 的 wire shape 与字段不变。错误前缀、非规范 UUID32、跨 session 投影或无法关联到 step 的 step-backed transcript 行不会进入 renderer 状态。数据库和 IPC DTO 未改变，无需数据重置。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm exec prettier --write` 涉及的 UI 文件
- `git diff --check`

## 回滚

若 Rust response 引入新的实体种类或合法 row linkage，应先更新 Rust DTO / ID 规范及本解析器测试，再扩展允许的前缀或关联关系；不要把未经校验的 command result 直接送入 reducer。
