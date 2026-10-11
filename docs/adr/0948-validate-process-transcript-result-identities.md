# 0948：校验 process_transcript 返回的 session 与 message 身份

## 状态

已接受并实现（2026-10-11）。

## 背景

`process_transcript` 的生成 TypeScript union 由 Rust DTO 导出，但 renderer 直接信任 `invoke` 的运行时值。`SessionCreated.session_id` 用于把 draft/旧 session 中的乐观输入迁移并选择新会话；两个 variant 的 `message_id` 会替换乐观气泡 ID，供后续 resume、rollback 与 transcript merge 使用。

## 决定

- 新增 `contracts/processResult.ts` 解析 command response 的 Rust externally-tagged `SessionCreated` / `Supplemented` variants。
- `SessionCreated.session_id` 必须为规范 `ses-*`；两个 variant 的可选 nullable `message_id` 若存在则必须为规范 `msg-*`。
- `submitTranscript` 先校验原始 `invoke` 结果，再允许它进入队列完成、session adoption 与消息 ID re-key；无效结果走现有 rejected-submit 清理路径。`processResultSessionId` 与 `processResultMessageId` 也仅返回规范身份。
- 保持合法 response 的 wire shape 不变；接受省略或 null 的 optional message ID，忽略额外的 additive 字段。

## 影响

格式错误或 variant 不明确的 command response 不会改变 reducer 的 session/message identity。数据库、IPC DTO、持久化语义与合法用户流程未改变，无需重置数据。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm exec prettier --write` 涉及的 UI 文件
- `git diff --check`

## 回滚

若 Rust 为 `ProcessResult` 增加 variant 或持久实体类型变更，需先更新生成 DTO、ID 规范和 parser 测试，再放行该变体；不要让原始 `invoke` 值直接选择会话或重写 message identity。
