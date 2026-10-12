# ADR 1054：规范化 UI 正向实体 ID 测试夹具

## 状态

已接受（2026-10-12）

## 背景

Rust 正向实体 ID 测试夹具已统一，但 UI 测试仍大量使用短 ID、裸消息 ID 和无法表达实体前缀的动态值，降低了跨端契约测试的有效性。复核 `agent:stream_reset` 时还发现 UI mapper 将 `reasoning_message_id` 按 `step-*` 校验，与 Agent 生产事件使用的 `msg-*` 不一致。

## 决定

- UI 正向测试夹具中的 session、message、step、ToolRun、asset、fact、recording 与 confirmation identity 使用各自的 `prefix-{uuid32}` 规范形状，并保持引用关系一致。
- Thought 与 Ask step 使用 `step-*`；Reasoning message 使用 `msg-*`。`agent:stream_reset` 的 mapper 按此规则分别验证两个字段。
- 计数器生成的测试 ID 使用固定宽度的小写十六进制；用于测试 UI 排序、缓存和容量的派生 stream segment、WebSearch card ID 与临时 sentinel 保持其各自的非实体语义。
- 明确验证格式拒绝、恶意/伪造输入、provider-owned ID 与临时 sentinel 的测试继续使用其原有语义，不将负例改成合法实体 ID。

## 影响与兼容

只调整测试夹具，并修正 UI 对既有 Agent event identity contract 的校验。没有更改 IPC 字段或序列化形状、数据库 schema、durable event 或配置格式，无需重置数据。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked`
- `cd ui && corepack pnpm run check`
- `cd ui && corepack pnpm run test:run`
