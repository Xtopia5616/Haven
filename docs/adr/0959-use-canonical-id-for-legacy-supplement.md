# 0959：为无消息身份的 committed supplement 使用规范 ID

## 状态

已接受并实现（2026-10-11）。

## 背景

`CommittedUiPublisher` 将提交的 `UserInject` transcript 投影为 Agent Supplement event。新写入的 UI annotation 会保存 producer 生成的 `msg-*` supplement ID；历史记录缺少 annotation 且没有 `message_id` 时，publisher 曾用 `msg-seq-{sequence}` 作为 fallback。UI event contract 对必需的 `supplement_id` 校验 `msg-*` canonical 格式，因此该 fallback 会被 mapper 丢弃，用户 steering/cross-session context 的 UI 通知无法显示。

## 决定

- 对没有持久 message identity 且没有可用 UI annotation 的 legacy supplement，使用 `haven_common::types::new_id("msg")` 生成符合既有 wire contract 的 projection identity。
- 保持已提交的新 UI annotation 优先；已有 `message_id` 仍直接复用，frontend 的 `supplement_id` 契约不变。
- 用回归测试覆盖没有 `message_id`/annotation 的 durable event，确认 emitted `supplement_id` 符合 `msg-*` 格式。

## 替代方案

- 保留 `msg-seq-*`：不符合 UI mapper 的 canonical ID 校验，事件继续被拒绝。
- 丢弃无 identity 的 supplement：会丢失应该由 committed event 投影的用户可见状态。

## 影响

只修复缺少旧 annotation 的 UI event fallback。正常写入事件仍复用保存的 annotation identity；没有 transcript、IPC schema、SQLite 字段或事件顺序变化，因此无需重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-agent supplement_without_a_message_id_gets_a_canonical_fallback_identity`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/check-adr-index.ps1`
- `git diff --cached --check`

## 回滚

如 frontend 合法 ID 契约将来变化，应同步更新 Rust event producer 与 UI mapper；不要恢复序列拼接的伪实体 ID。
