# 1028：统一 Memory 领域模型身份字段

## 状态

已采纳并实施（2026-10-12）。

## 背景

Memory 对外可见的 Rust 领域模型 `Session`、`Message` 和 `Fact` 分别以通用字段 `id` 暴露实体身份，与仓库的 `xxx_id` 命名规范及其它 crate 的显式身份字段不一致。调用方需要结合模型类型才能推断 `id` 的含义。

## 决定

- 将 `Session.id`、`Message.id`、`Fact.id` 分别改为 `session_id`、`message_id`、`fact_id`，并更新 Memory、Agent、Tools、App 及其测试中的 Rust 引用。
- Repository 继续将这些显式领域字段映射到现有 SQLite `id` 列；不改变 schema、查询、实体 ID 格式或存储数据。
- 保留事实抽取提示词 JSON 中约定的 `id` key，值从 `Fact.fact_id` 读取。该 key 是模型输入协议，不是 Rust/数据库身份字段。
- `Message.message_id` 不额外限制为 `msg-*`，因为 transcript 中的消息可以合法复用 `step-*` 执行步骤身份。
- 领域模型的 Serde 字段现在序列化为 `session_id`、`message_id` 与 `fact_id`；它们没有被用于生产持久化或 IPC，故不引入数据重置或跨端迁移。

## 影响

Rust 调用方必须按实体身份字段读取这三个模型。SQLite 物理列、session events、Tauri 命令/事件 DTO 与 UI wire shape 均保持不变。新增序列化断言固定领域模型字段名，避免重新暴露含义不清的 `id`。

## 验证

- cargo fmt --all -- --check
- cargo test --workspace --locked
- cargo check --workspace --locked
- cargo clippy --workspace --locked -- -D warnings
- git diff --check
- IPC 生成与漂移检查（无 wire 变化）

## 回滚

恢复三个领域模型字段及 Rust 调用点即可。无需数据库重置；不要将 SQLite 的物理 `id` 列或事实抽取 prompt JSON key 随领域字段一起改名。
