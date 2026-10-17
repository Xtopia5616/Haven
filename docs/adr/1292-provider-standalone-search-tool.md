# ADR 1292：将 Provider 独立搜索作为模型级工具

## 状态

已实现。新增可选模型配置字段，不需要数据库重置；旧配置因字段可选且默认为空继续有效。

## 背景

DeepSeek Search & Fetch、Perplexity Search、Mistral Conversations Web Search 与智谱 Web-Search-Pro 是可单独计费或使用独立端点的搜索产品，不等同于原生 Chat 搜索开关。它们有不同的请求字段、结果格式和调用约束。原生搜索设置无法表达这些产品，也不应把 vendor wire 格式放进通用请求构造器。

## 决定

- 在每个 `ModelConfig` 增加可选的 tagged `independent_search` 配置。UI 只对 DeepSeek、Perplexity、Mistral、智谱 Provider 显示对应设置；切换 Provider 时清除原 Provider 的选项。配置必须与被选 Provider 身份匹配。
- 将模型可调用入口命名为 `web_search_api`。仅当 Chat 路由模型配置独立搜索且工具没有被关闭时注册。工具调用通过当前模型的 Provider API 执行并返回可持久化的搜索结果卡；Mistral 答案正文与来源引用一并显示。
- 各 Provider 请求与响应协议放在各自 `haven-llm` adapter 文件。公共模块只负责有界 HTTP 传输、取消、结果字段截断和 HTTP(S) 来源过滤。
- DeepSeek 使用 `/v1/tools/search`，传 `count`、`need_content`，查询限制 128 字符；Perplexity 使用 `/search`，支持 `web`、`fast`、`people`、结果数和发布时间范围，按官方上限验证结果数（web/fast ≤20，people ≤50）；Mistral 使用 `/v1/conversations` 和 `web_search` / `web_search_premium`，不经过 Chat Completions；智谱使用 `web-search-pro` Chat Completions 并解析 `tool_calls[*].search_result`。
- 搜索工具的查询长度上限 512 字符；单次响应最多读取 4 MiB、最多同时执行两个独立搜索。传输超时保留 Provider 更长的超时配置，至少 30 秒且最多 120 秒；工具外层超时为 120 秒并响应取消。搜索操作声明只读与 Public network，继续经过已有网络授权策略；错误输出不包含 Provider 原始响应或密钥。
- 配置解析对 DeepSeek 数量、Perplexity 数量/类型和 Provider 身份进行 Rust 与 UI 双侧校验。独立搜索启用时关闭该模型的内置搜索模式，避免一次配置同时表达两种不同产品。

## 替代方案

- 复用 `web_search` 原生模型参数：拒绝。独立产品有额外 endpoint、计费和参数，调用语义不同。
- 由通用 OpenAI-compatible adapter 按 Provider 字符串拼请求：拒绝。Mistral Conversations 与智谱 Search Pro 的协议/返回结构并非同一种 Chat 请求；DeepSeek 和 Perplexity 也有自己的 Search API。
- 保存搜索选项但不进入工具调用：拒绝。启用选项应能通过模型工具目录到达真实 Provider 请求。

## 影响与回滚

- 配置契约新增可选字段，serde 默认值保持旧配置兼容；没有数据库、session event 或 transcript schema 变更。工具观察结果通过既有 renderer 与 transcript 投影保存。
- 工具默认只在配置存在时注册；Provider 不匹配或配置无效时拒绝加载/执行。关闭 `tool_settings.web_search_api` 会同时移除该工具并从 prompt 能力中隐藏独立搜索。
- 回滚时移除模型字段、独立搜索 adapters/tool 和 UI 控件即可。若保留用户配置文件，未知字段会按当前配置契约拒绝，回滚前应删除 `independent_search` 项；本项目测试版本没有旧二进制回读保证。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `pwsh -NoProfile -File scripts/generate-ipc-contracts.ps1`
- `pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`
- `cd ui; corepack pnpm run check; corepack pnpm run test:run; corepack pnpm run build`

## 官方依据

- [DeepSeek Search API](https://api-docs.deepseek.com/api/search/)
- [Perplexity Search API](https://docs.perplexity.ai/api-reference/search-post)
- [Mistral Web Search](https://docs.mistral.ai/studio/agents/agent-tools/websearch)
- [Mistral Conversations API](https://docs.mistral.ai/api/endpoint/beta/conversations)
- [智谱 AI 搜索引擎 / Web-Search-Pro](https://docs.bigmodel.cn/cn/best-practice/case/ai-search-engine)
