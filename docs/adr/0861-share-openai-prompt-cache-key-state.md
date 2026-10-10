# ADR 0861：共享 OpenAI prompt cache key 支持状态

## 状态

Accepted — 2026-10-10

## 背景

`OpenAiAdapter` 与 `OpenAiResponsesAdapter` 各自实现了相同的 prompt cache key 兼容状态：三个原子状态、拒绝后的 300 秒重探测期限、成功后恢复启用，以及通过 `RequestFailed` 错误文本识别 provider 不支持该字段。两处 `current_epoch_seconds` 实现也与 Gemini 缓存重试使用的实现完全相同。重复状态机可能让不同协议分支采用不一致的重探测或拒绝条件。

Chat Completions 与 Responses 的 cache key 内容、provider 允许集合和失败后的请求降级仍有协议差异，应继续归各自 adapter。

## 决定

- 在 `adapters/openai/prompt_cache_key.rs` 唯一拥有 `PromptCacheKeySupport`。每个 OpenAI Chat 或 Responses adapter 实例各自持有一个状态对象，不在实例间共享支持结果。
- `PromptCacheKeySupport` 唯一拥有 `UNKNOWN`、`ENABLED`、`UNSUPPORTED` 状态与 300 秒重探测策略：非 `UNSUPPORTED` 状态允许附带 key；不支持状态仅在非零期限已到时再次附带；拒绝时记录 `now + 300s`，成功时清期限并恢复 `ENABLED`。原子读写继续使用 `Relaxed` ordering。
- OpenAI Chat 与 Responses 共用 `is_unsupported_prompt_cache_key_error`，保持只识别 `RequestFailed` 且要求错误文本同时包含 `prompt_cache_key` 与既有拒绝提示词的条件。
- `adapters::current_epoch_seconds` 是 LLM adapter expiry/retry window 的唯一当前 Unix 秒读取。OpenAI 与 Gemini 继续各自持有期限长度和业务状态。
- Cache key 生成、provider allowlist、Responses developer-input 降级及请求 body 修改仍由协议 adapter 拥有。
- 不改变 wire payload、key 内容、300 秒间隔、provider 错误识别条件或恢复行为。

## 替代方案

- 保留 Chat 与 Responses 两份状态机：拒绝。它们是同一 OpenAI 扩展的同一兼容性状态 owner。
- 把 key 生成与请求降级整体合并：拒绝。两种协议的 wire 形状与降级行为不同，合并会模糊协议 owner。
- 把时钟与所有 provider 的缓存重试策略合并：拒绝。只共享无策略的当前时间读取；Gemini 和 OpenAI 的期限与状态语义仍不同。

## 影响与验证

这是 `haven-llm` 内部状态与纯函数的 owner 收敛。每个 adapter 的状态仍独立；没有 IPC、持久化或用户配置变化。更新现有 OpenAI/Responses 测试以经共享 owner 操作兼容状态和错误分类；本轮未运行测试套件。

验证通过：指定文件 `rustfmt --check`、`cargo check -p haven-llm --locked`、
`cargo check -p haven-llm --tests --locked`（编译测试目标但未执行测试）与 `git diff --check`。

## 回滚

若某一 wire 协议需要不同的拒绝识别或重探测语义，应由该协议定义命名明确的策略，而不是复制当前状态机。回滚时恢复两个 adapter 的内联状态和 classifier，并删除该共享模块；无持久数据需要重置。
