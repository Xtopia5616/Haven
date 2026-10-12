# ADR 1053：规范化 Rust 正向实体 ID 测试夹具

## 状态

已接受（2026-10-12）

## 背景

ID 规范和各边界校验已统一，但 Rust 测试中的正向夹具仍混用短 ID、非十六进制后缀和未加前缀的 UUID。此类夹具会掩盖生产数据形状与测试数据之间的差异，也会让按实体关联的测试无法覆盖真实契约。Thought 与 Reasoning 共用部分事件字段，但身份空间不同：Thought 复用执行步骤的 `step-*`，Reasoning 使用 `msg-*`。

## 决定

- Agent、App、Common、LLM、Memory、Messaging 和 Tools 的正向实体 ID 测试夹具使用对应的 canonical `prefix-{uuid32}` 形状。
- Thought 消息/流式事件夹具使用 `step-*`，Reasoning 夹具使用 `msg-*`；关联的 StreamReset、payload 断言与 transcript 夹具保持同一身份。
- 按计数器生成的测试身份使用 32 位小写十六进制格式；需要随机消息身份的测试辅助函数使用 `haven_common::types::new_id("msg")`。
- 明确验证拒绝行为的 malformed、forged、provider-owned ID 和普通测试文本不改写为正向实体 ID。

## 影响与兼容

仅调整测试夹具和注释，不改变生产行为、数据库 schema、durable event、IPC、配置或文件路径契约。无需数据库或配置重置。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked -- --test-threads=1`
