# ADR 1290：更新 Provider 原生搜索适配与引用回显

## 状态

已实现。无需重置配置或数据库。

## 背景

各 Provider 的搜索能力继续沿用相近名称，但请求协议、模型白名单和返回引用结构已有变化。仅按 `api_style` 判断是否能搜索，会把不支持该功能的模型也当成可用；只解析搜索调用而忽略最终回答上的引用注释，则会丢失可展示来源。

当前接入边界如下：

| Provider | 现有/当前协议 | 搜索处理 |
|---|---|---|
| OpenAI | Responses API | 使用 `web_search`，解析搜索调用与 `url_citation` 注释 |
| xAI | 新文档推荐 Responses API；旧配置仍有 Chat Completions | 新预设切到 `web_search`；保留旧 `search_parameters` 配置路径 |
| Anthropic | Messages API | 使用 `web_search_20260318`，显式设置直接调用和完整结果回显；保留搜索调用与结果块，包括 `encrypted_content` |
| Google Gemini | GenerateContent | 保持 `google_search`，并从 grounding metadata 提取查询、来源与对应片段 |
| DeepSeek | Responses API | 仅 `deepseek-v4-pro` 暴露 `web_search`；其它模型不发送不支持的工具 |
| Groq | OpenAI 兼容 Responses API | 新预设使用 `browser_search`；仅 GPT-OSS 20B/120B 且未启用结构化输出时开放 |
| Perplexity | Sonar Chat Completions | 保留当前顶层 `citations` 解析与搜索来源投影 |
| Mistral | Haven 当前配置为 Chat Completions | 暂不开放；Provider 的 `web_search` 需要 Conversations/Agents API 及其引用输出 |

## 决定

- 扩展现有 `web_search_calls` 解析，将 OpenAI-compatible Responses 文本块的 URL citation 注释投影为标准搜索来源；兼容 Groq `browser_search_call` 输出名称。
- xAI 与 Groq 新建预设分别选择其官方文档支持的 Responses API 搜索工具。既有已保存的 xAI `api_style: xai` 配置继续由旧 Chat Completions / `search_parameters` adapter 处理。
- Provider / model 的限制在 LLM adapter 内判定。DeepSeek 仅对 `deepseek-v4-pro` 注入搜索；Groq Responses 仅对 GPT-OSS 20B/120B 注入 `browser_search`，并在结构化输出启用时关闭该能力。
- Anthropic 采用当前搜索工具版本，并将 `allowed_callers: ["direct"]` 与 `response_inclusion: "full"` 写入工具定义。对话回放保留原始 `server_tool_use` 与 `web_search_tool_result` 块，避免丢弃完整结果和必须回显的加密内容。
- Gemini 的 grounding 结果不仅保留 `webSearchQueries`，还从 `groundingChunks` 与 `groundingSupports` 投影来源 URL、标题和相关文本片段。忽略未知 chunk 字段，避免把大型/非引用元数据带入 transcript。
- Mistral 需要独立的 Conversations/Agents 协议、状态与引用映射；不把 Conversations 的工具字段塞入现有 Chat Completions 请求，也不在本次建立有状态远端会话。

## 替代方案

- 只依赖 `api_style` 宣告搜索能力：拒绝。DeepSeek 与 Groq 都有模型白名单，Groq 搜索也不能与结构化输出并用。
- 将 xAI 的所有既有配置强制切到 Responses：拒绝。用户已有 Chat Completions 配置依赖旧适配器的搜索字段与会话关联；只有新预设选择新协议。
- 让 Mistral Chat Completions 接收 Conversations API 的 `web_search` 工具：拒绝。官方文档明确说明 Chat Completions 不返回该工具所需的引用结果。
- 为 Perplexity 新建一套搜索协议：拒绝。本次目标是更新现有 ReAct 路由；Sonar Chat Completions 已返回 `citations`，独立 Search/Agent API 需要另行定义产品交互与运行时协议。

## 影响与回滚

- transcript 已有 `web_search_calls` 和 provider-opaque `thinking_blocks` 字段承载搜索调用与回放块；不增加持久化字段、配置字段或 schema 版本，无需重置。
- 搜索状态 UI 继续使用现有 `WebSearchUpdate` 和 citation 投影契约。
- 回滚时恢复对应 adapter、预设和 capability 变更即可；Anthropic 原始搜索块由现有 transcript 数据结构承载，不需要数据库迁移。

## 官方依据

- [OpenAI Responses web search](https://developers.openai.com/api/docs/guides/tools-web-search)
- [xAI Web Search](https://docs.x.ai/developers/tools/web-search)
- [Anthropic Web Search tool](https://platform.claude.com/docs/zh-CN/agents-and-tools/tool-use/web-search-tool)
- [Gemini grounding with Google Search](https://ai.google.dev/gemini-api/docs/google-search?hl=en)
- [DeepSeek Responses API](https://api-docs.deepseek.com/guides/responses_api/)
- [Groq Responses API](https://console.groq.com/docs/responses-api)
- [Perplexity API overview](https://docs.perplexity.ai/discuss/66b8de9e88a46a004b5c812e)
- [Mistral Websearch](https://docs.mistral.ai/studio/agents/agent-tools/websearch)
