# ADR 1291：按 Provider 隔离原生搜索实现

## 状态

已实现。无需重置配置或数据库。

## 背景

ADR 1290 更新了 OpenAI Responses、xAI、Anthropic、Gemini、DeepSeek、Groq、Perplexity 与 Mistral 的搜索适配。复核各 Provider 当前文档后，又发现 OpenAI Chat 专用搜索模型已更新，Groq 也提供 Chat Completions 浏览器搜索；OpenRouter 与 DashScope 的原生搜索协议需要独立参数、模型限制和结果处理。把这些差异继续堆在通用 OpenAI-compatible 请求构造器中，会让 Provider 身份和 wire 语义难以审查。

## 决定

- 保留公共 `off|auto|always` 设置和通用搜索结果 DTO；新增 `adapters/openai/search.rs`、`dashscope.rs`、`deepseek.rs`、`groq.rs`、`openrouter.rs`、`xai.rs`，分别实现厂商自己的模型支持范围、请求参数、强制策略、用量含义和引用投影。
- OpenAI Chat 使用当前专用 `gpt-5-search-api` 搜索模型与 `web_search_options`；停止将已下线的 `gpt-4o-search-preview` 与 `gpt-4o-mini-search-preview` 作为可用搜索模型。
- OpenRouter Responses 使用 `openrouter:web_search`，Chat Completions 使用命名空间工具；不再采用已弃用的 `:online` 与 `plugins:[{id:"web"}]` 形式。OpenRouter 服务端决定是否搜索，因此 `always` 不强制调用。
- DashScope Chat 使用 `enable_search` 与 `search_options`，`always` 映射到 `forced_search`；需要 agent 搜索策略的 Qwen Omni 型号单独设置策略。`usage.x_tools.search.count` 按意图检测次数解释，不当作实际检索次数或账单用量。Responses 使用其 `web_search` tool 并由模型决定是否调用。
- Groq Chat 使用 `browser_search`，仅对官方支持的 GPT-OSS 模型开放；`always` 仅在没有用户工具冲突时要求搜索工具。Groq Responses 继续使用其 `browser_search` 工具并按支持模型限制能力。
- DeepSeek 直接 Responses 端点仅对文档支持搜索的 `deepseek-v4-pro` 开放。xAI Search 与 X Search 仍是分离功能；现有联网搜索开关不隐式启用 X Search。
- Mistral 的 `web_search` / `web_search_premium` 仍要求 Conversations/Agents API；其 Chat Completions cookbook 新增的是自定义 connector 示例，Web Search 页面仍明确说明 Chat Completions 不返回搜索引用。Haven 不因此把 Mistral 当作通用 Chat 原生搜索 Provider。
- 智谱的 `web-search-pro` 是单独的搜索模型，返回自有 `search_result` 结构；旧 Assistant/应用推理路线依赖 assistant 或 app/conversation 身份，部分 API 已标为 deprecated。它们不等同于普通 Chat 请求可注入的搜索工具，本次不把它们伪装成 `off|auto|always` 的模型级内置搜索。
- `capabilities.rs` 只保留公共模式解析、能力分发和中性请求装配，不持有某一个 Provider 的模型白名单或字段映射。共享 `web_search.rs` 仅归一化调用和引用，不猜测 Provider 的 usage 语义。

## 替代方案

- 按 `api_style` 给所有 OpenAI-compatible 模型开放相同搜索：拒绝。OpenAI、Groq、OpenRouter、DashScope 和 DeepSeek 的搜索字段、模型范围、引用格式和强制语义并不相同。
- 把所有特例继续写进 `openai/request.rs` 与 `capabilities.rs`：拒绝。它会让通用构造器拥有供应商模型政策，也难以跟随单一 Provider 文档更新。
- 把 X Search 合并为普通联网搜索：拒绝。它是单独的 Provider 工具能力，启用方式与费用语义不同。

## 影响与回滚

- 只调整模型请求与响应映射、Provider 预设和测试；没有新增持久字段或数据库表，既有 `web_search` 配置值继续有效，不需要配置迁移或数据库重置。
- 回滚时恢复各 Provider 搜索模块、分发逻辑、preset 与相关测试即可；不需要数据修复。

## 验证

- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`

## 官方依据

- [OpenAI Web search](https://developers.openai.com/api/docs/guides/tools-web-search)
- [OpenRouter Web Search Server Tool](https://openrouter.ai/docs/guides/features/server-tools/web-search)
- [Alibaba Cloud Model Studio Web Search](https://help.aliyun.com/en/model-studio/web-search)
- [Groq Browser Search](https://console.groq.com/docs/tool-use/built-in-tools/browser-search)
- [DeepSeek Responses API](https://api-docs.deepseek.com/guides/responses_api/)
- [xAI Web Search](https://docs.x.ai/developers/tools/web-search)
- [Mistral Websearch](https://docs.mistral.ai/studio/agents/agent-tools/websearch)
- [Mistral connectors in Chat Completions](https://docs.mistral.ai/resources/cookbooks/mistral-connectors-05-connectors-in-completions)
- [智谱 AI 搜索引擎 / Web-Search-Pro](https://docs.bigmodel.cn/cn/best-practice/case/ai-search-engine)
