# ADR 0431：Provider-specific tool name projection

## 状态

已接受（2026-10-03）。

## 背景

ADR 0137 将内置 operation 的模型可见名称统一为 `root.operation`，例如
`files.read`。这类名称直接进入 tool schema。Provider 对函数名的字符集和长度有各自约束；
DeepSeek 在 2026-10-03 返回 400，拒绝了包含点号的工具名，格式为
`^[a-zA-Z0-9_-]+$`。

回归由 2026-09-13 的 dotted operation view 改动引入：它改变了 canonical tool name，
但当时 `haven-llm` 的 wire adapter 仍将函数名原样发给 provider。之前 provider 收到的旧名
符合字符集，因此没有触发此错误。

## 决定

1. Haven 内部 `ToolDefinition`、权限 key、工具执行名和 Agent 收到的 `CanonicalToolCall.name`
   继续使用 canonical name；不得为了某个 provider 修改全局 operation 命名。
2. 各 provider adapter 在 `haven-llm` 的请求边界生成本次请求的 name map：声明的工具名以及
   历史 assistant tool call 名一起映射，以保证重放的调用名、工具定义和工具结果引用一致。
   Provider 返回的同步及流式 tool call 名在离开 adapter 前恢复为 canonical name。
3. 映射只对有约束的 provider/protocol 生效：把不允许的字符替换为 `_`；遇到归一化冲突或
   超长名时，使用确定性短摘要后缀产生唯一 alias。未冲突且长度合规的名称保持不变。
   请求 map 覆盖当前工具和历史调用，因此同名归一化冲突不会让调用路由到错误工具。
4. 按 provider 文档使用各自长度界限，而不是把 DeepSeek 限制成 OpenAI 的 64 字符上限：
   - OpenAI Chat Completions / Responses：64 字符。
   - DeepSeek Chat Completions / Responses：128 字符。
   - Anthropic Messages：128 字符。
   - Gemini：128 字符。Gemini 的 declaration 文档比 call/response 名字段接受更多标点，
     因此取后两者共同支持的字符集以确保往返。
   - xAI：文档要求工具名唯一但没有规定字符集或长度；保留名称原样，不推断 OpenAI 的
     更严格规则。
   OpenAI-compatible wire style 默认遵循 OpenAI 的函数名字符集和长度；已明确记录差异的
   provider（如 DeepSeek、xAI）使用自己的策略。未明确记录差异的 gateway 继续使用兼容协议
   默认值；若其文档定义了不同规则，应在 provider profile 中单独覆盖，不能借用别家的限制。
5. Provider alias 只在 adapter 请求/响应生命周期内使用，不持久化，不进入权限 key、UI、
   session transcript 或执行器。Provider 返回未登记名称时保留原值，避免丢弃未知数据。

## 替代方案

- 将 canonical name 改回下划线：拒绝。会与 ADR 0137 的权限、renderer 和工具契约名称不一致。
- 对所有 provider 套用同一长度限制：拒绝。DeepSeek、Gemini 和 xAI 的公开契约并不完全相同。
- 只修 DeepSeek Responses 请求：拒绝。Anthropic、Gemini、OpenAI 及兼容协议也可能拒绝点号，
  且流式与多轮 tool history 必须保持相同的双向映射。
- 遇到名称冲突时直接拒绝请求：拒绝。确定性 alias 可保持当前操作可用，同时避免错误路由。

## 影响与验证

仅改变 provider wire tool names；不改变内部工具契约、配置、数据库 schema 或 transcript。
请求缓存 fingerprint 使用实际 wire schema，避免 alias 和实际工具定义不一致。

实现位置：`crates/llm/src/tool_names.rs` 与各 provider adapter 的 request/mapping/stream 边界。
质量门禁为 `cargo fmt --all -- --check`、`cargo check --locked -p haven-llm`、严格 Clippy，
以及现有 provider adapter 测试。

参考文档：

- [OpenAI function name contract](https://github.com/openai/openai-java/blob/main/openai-java-core/src/main/kotlin/com/openai/models/FunctionDefinition.kt)
- [DeepSeek Chat Completions API](https://api-docs.deepseek.com/api/create-chat-completion/)
- [DeepSeek Responses API](https://api-docs.deepseek.com/api/create-response/)
- [Anthropic define tools](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools)
- [Gemini generateContent API](https://ai.google.dev/api/generate-content)
- [xAI function calling](https://docs.x.ai/developers/tools/function-calling)

## 回滚

回滚本 ADR 对应 adapter 与 name-map 改动即可；无需数据库、配置或 transcript 重置。
