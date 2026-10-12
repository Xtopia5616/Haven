# ADR 1047：在交互事件持久化前校验身份

## 状态

已接受（2026-10-12）

## 背景

Session interaction replay 校验 request、session、step、tool-run 和 confirmation receipt identity 的规范格式及关联关系。Actor 的 `interaction_requested` 写路径此前只验证 session 归属；malformed interaction identity 因而可能成功进入 `session_events`，并在下一次恢复时让整段 interaction replay 失败关闭。

## 决定

- durable interaction identity 由 `validate_interaction_ids` 统一校验；写入与 replay 使用同一个实现。
- 单个交互请求在 append 前校验身份和 session 归属。
- confirmation batch 在构建持久事件前校验每个请求的身份和 session 归属；任一无效请求都拒绝整个 batch。
- 保留 replay 校验，继续对历史或损坏事件 fail-closed。

## 影响与兼容

仅改变新写入的拒绝边界，不更改有效事件格式、schema 或恢复语义。过去已经写入的 malformed event 仍由 replay 拒绝；本修复不尝试猜测或修复其身份。无数据库迁移或重置要求。

## 验证

- Actor 单请求测试覆盖错误 session 归属及 malformed step identity，并确认拒绝后事件流不变。
- confirmation batch 测试覆盖 malformed step identity，并确认不 append、不暂停 session。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 与 `cargo test --workspace --locked`。
