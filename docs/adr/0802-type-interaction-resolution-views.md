# ADR 0802：类型化 interaction resolution views

## 状态

已采纳并实施（2026-10-08）。

## 背景

Ask 的 renderer-local response 在 `InteractionRequest` 中是 `unknown`，两个读取点分别强制转换为 `AskResponseView`。Confirmation resolution 的 reducer action 也重复声明了 Rust command 返回值 `'resolved' | 'expired' | 'stale'`，并把未被读取的 `{ approved, effect, scope }` 对象以 `unknown` 附加到 interaction。

## 决定

1. `InteractionRequest.response` 与 `session/interaction-resolved.response` 使用 `AskResponseView`，消费者直接读取类型化 view，不再 cast。
2. `session/interaction-resolution-result.result` 复用生成的 `ConfirmationResolutionResult`。
3. 删除没有消费者的 confirmation response 附加对象与 reducer action 字段；confirmation 的状态仍由生成结果驱动。

## 影响与回滚

仅收紧 renderer 内存状态与 reducer action 类型。Tauri command/event wire、confirmation status 迁移和 Ask 展示行为不变；被删除的确认详情对象此前没有读取方。无持久化或配置变化。

## 验收

运行 UI 类型检查与完整 UI 测试、ADR 索引检查；无 contract generator 影响。
