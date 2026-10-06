# ADR 0572：让 session 状态工具直接使用生成契约

## 状态

已采纳并实施。

## 背景

`sessionStatus.ts` 负责 session status 判断、waiting reason 归一和 UI 展示映射。它原样重导出 generated `SessionStatus`、`SessionWaitingReason` 以及两组 value arrays，但没有其它生产模块消费这些导出；模块自身只需要 generated waiting reason 类型和值，测试则经该 utility 间接检查 enum vocabulary。

## 决定

1. 状态展示 utility 直接从 `generatedCommands.ts` 导入 `SessionWaitingReason` 与 `SESSION_WAITING_REASON_VALUES`。
2. 删除未被其它模块消费的 status type/value aliases；测试直接断言 generated `SESSION_STATUS_VALUES` 和 `SESSION_WAITING_REASON_VALUES`。
3. `isSessionWaitingReason` 只被同模块的展示/归一函数使用，收为私有 helper；对外保留有实际消费者的 status 判断、等待原因归一与标签函数。

## 替代方案

- 保留 utility 对 generated enums 的 facade：拒绝，这些 aliases 没有 renderer 形状变化或额外领域约束，也没有跨模块消费者。
- 将全部判断与标签搬入 generated contract：拒绝，generated contract 只拥有 Rust wire 值，不应包含 UI 文案和展示策略。
- 让消费者导入 `sessionStatus.ts` 的 status type alias：拒绝，状态类型的 owner 是 generated IPC contract。

## 影响与验证

- 只收窄 UI 内部 TypeScript exports；wire values、用户文案、归一策略与组件行为不变。
- 无 Rust、Tauri command/event、数据库或配置契约变化。
- 验证：UI `check`、`test:run`、`build`、ADR 索引与差异空白检查。

## 回滚

恢复 `sessionStatus.ts` 的 aliases/value re-exports 与公开 type guard，并让测试重新从该模块导入 status arrays。
