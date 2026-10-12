# ADR 1046：区分 provider tool-call 身份与流式预览身份

## 状态

已接受（2026-10-12）

## 背景

Agent 把 provider 返回的 `tool_call_id` 写入 durable transcript、`session_steps`、确认/重试关联，并在 resume 后送回 provider history。当前 parser 使用 `trim()`，会改写不透明的 provider 值，也会把仅首尾空白不同的 ID 合并。provider ID 缺失或同一 batch 重复时，Agent 生成的 `call-*` fallback 同样被持久化；它不是进程内 ID。

流式工具参数预览此前也使用 `call-*`，但 preview 只用于当前进程的临时 UI 更新。相同前缀让两种生命周期看起来相同，并与 ID 规范和 ADR 0059 中的“进程内调用 ID”描述冲突。

## 决定

- `step_id + tool_index` 是 Haven 的稳定执行身份；`tool_call_id` 保留 provider history 配对用途。
- 将 provider `tool_call_id` 视为不透明字符串，按原值保留，不 trim、不做大小写或格式规范化。仅空字符串和同一 assistant tool batch 内已使用的重复值生成 fallback。
- fallback 使用 `haven_common::types::new_id("call")`，加入当前 batch 的已用 ID 集合，并随 transcript、步骤、确认/重试关联持久化；resume 与 provider history 保留已有值，不从 `call-*` 前缀推断身份来源。
- 流式工具调用参数预览使用独立的进程内 `preview-*`。预览 ID 不进入 transcript 或持久化步骤。
- 历史 ADR 0059 保留为决策记录；本 ADR 取代其中关于 `call-*` 是进程内 ID 的描述。

## 替代方案

- 继续 trim provider ID：会改写 opaque identity，并可能合并本来不同的调用，拒绝。
- 用 `call-*` 同时标识 durable invocation fallback 与临时 preview：无法从前缀区分生命周期，拒绝。
- 将所有缺失/重复 ID 改为仅限 provider request 的临时 ID：需要重新设计确认、重试、canonical transcript 和恢复时的 call/result 配对；当前 `step_id + tool_index` 已提供独立 Haven 执行身份，因此暂不扩展该重构。

## 影响与兼容

provider ID 与 fallback 的持久字段形状不变；不改 database schema、durable event payload 或 reset 要求。预览 event 的 `preview_id` 仍是字符串，但前缀从 `call-*` 改为 `preview-*`，UI contract 同步校验新前缀。旧 transcript 中的 `call-*` 值继续当作原始 `tool_call_id` 回放，不作重写或分类。

## 验证

- Rust 单测覆盖带首尾空白的 provider ID 精确保留，以及同一 batch 重复 ID 生成 `call-*` fallback。
- UI contract 测试验证 `preview-*` 可接受、`call-*` 不能作为 preview ID 接受。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`corepack pnpm run check` 通过。
- `corepack pnpm run test:run` 通过（145 个文件，1212 项）。首轮并行 `cargo test --workspace --locked` 有一个 app-binary tracing 捕获测试未包含 phase metadata；该测试单独复跑通过，完整 `cargo test --workspace --locked -- --test-threads=1` 通过。
