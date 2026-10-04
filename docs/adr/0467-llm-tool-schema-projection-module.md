# ADR 0467：将 Provider 工具 Schema 投影归入 adapter 边界

## 状态

已采纳，实施中（2026-10-05）。

## 背景

`crates/llm/src/types.rs` 同时定义稳定的请求/工具数据类型、通用 schema 净化与 JSON canonicalization，还包含约 600 行 OpenAI-compatible object-root union 投影和 Gemini OpenAPI subset 投影。后两者只为 provider 请求整形，却被放在共享类型模块；adapter mapping 通过 crate 根层 `types` 间接调用，职责归属不明显。

这些算法已有独立纯函数边界和投影测试。OpenAI Chat 与 OpenAI Responses 共用 object-root 投影；Gemini 在 object-root widening 基础上再做其 schema subset 投影。Anthropic 只需要通用净化。工具执行的输入校验仍使用 `haven-tools` 持有的完整工具 schema，不依赖这些 provider 视图。

## 决定

1. 将 object-root 投影、Gemini subset 投影及它们专用的内部 helper/纯算法测试移入 `crates/llm/src/adapters/tool_schema.rs`，作为 adapter 私有模块，不导出 crate 公共 API。
2. OpenAI Chat、OpenAI Responses 和 Gemini mapping 显式调用该 helper；每个 adapter 继续选择自己的 wire 方言及调用时机。
3. 将 `ToolDefinition`、`ToolDefinition::from`、通用 `sanitize_tool_parameters`、schema sanitizer helpers、`canonicalize_json` 与 `stable_json_bytes` 留在 `types.rs`。Anthropic 继续只做通用净化。
4. 保持投影实现和 adapter mapping 的字段、分支顺序与 canonicalization 不变；本次是源码 owner 调整，不改变 provider tool-schema wire 输出、缓存身份、工具调用解析、完整 schema 执行校验或持久数据。

## 替代方案

- 把 schema 投影留在 `types.rs`：拒绝。共享数据类型模块继续承担只被部分 provider 使用的协议方言算法，边界问题不变。
- 为 OpenAI Chat、Responses 和 Gemini 各复制一份 object-root 算法：拒绝。两条 OpenAI 路径必须保持一致，Gemini 的 object-union 也复用相同 root widening；复制会形成多个行为 owner。
- 新建 provider 公共 crate/API：拒绝。消费者只有 `haven-llm` 内部 adapters，没有独立依赖收益。

## 影响与验证

模块保持私有，调用 API 不变；没有配置、IPC、schema 或持久化变化，无需用户数据重置。重点验收 provider 转换测试、现有投影算法回归和 prompt-cache canonical projection 行为保持不变。

实施验证结果将在代码切片完成后补录；适用门禁为 `cargo fmt --all -- --check`、`cargo test --locked -p haven-llm`、`cargo clippy --locked -p haven-llm -- -D warnings` 及 `git diff --check`。

## 回滚

将 adapter 私有 helper 与纯算法测试移回 `types.rs`，恢复三处 mapping 调用路径即可；无数据迁移或用户状态重置。
