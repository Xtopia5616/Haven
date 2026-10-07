# ADR 0622：命名 OpenAI Responses input conversion 结果

## 状态

已采纳并实施。

## 背景

OpenAI Responses adapter 的 `convert_input` 与 `convert_input_with_memory_split` 共用 canonical transcript mapping，生成 provider `input` items，并把系统 instructions 单独生成供 request 使用。常规和 developer-input 不可用两种路径由 request builder 与 guidance append 共用；tests 原通过 `(input, instructions)` tuple 读取结果。

## 决定

1. 定义 `ResponsesInputConversion { input, instructions }` 作为两种 conversion 路径的共享返回类型。
2. Request builder、guidance append 和测试按具名字段访问。
3. 保持 cache-stable instructions、dynamic developer/user fallback、reasoning echo、tool mapping 与 wire JSON 不变。

## 替代方案

- 两个 conversion 各定义一个类型：拒绝，它们返回相同 Responses request projection，仅 developer input fallback policy 不同。
- 复用 Gemini/Anthropic conversion DTO：拒绝，这些 adapter 各自拥有不同 wire item 与系统提示 shape。
- 把 instructions 合并到 input items：拒绝，会改变 Responses API request contract 与稳定 instruction prefix。

## 影响与验证

- 这是 `haven-llm` OpenAI Responses adapter 内部 Rust 类型调整，provider wire 与恢复语义不变。
- 命名审计 §5.7 保持 Active；其它 provider adapter 和全项目符号仍待逐域审计。
- 验证：LLM fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(input, instructions)` 返回并同步两种 conversion、request builder、guidance append 和 tests；无需 wire 或持久化迁移。
