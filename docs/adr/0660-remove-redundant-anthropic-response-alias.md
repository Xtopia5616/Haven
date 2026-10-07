# ADR 0660：删除 Anthropic 响应中重复的 serde alias

## 背景

`AnthropicResponse.stop_reason` 未声明 `rename_all` 或自定义 `rename`，Serde 的规范输入字段本来就是 `stop_reason`。原字段又写了 `#[serde(alias = "stop_reason")]`，alias 与主字段名完全相同，没有新增可解析格式或转换语义。

## 决定

- 删除重复 alias，保留标准 `stop_reason` 字段解析。
- 保留其它拼写不同的 Anthropic/OpenAI/Gemini/MCP wire aliases；它们映射各自的外部协议字段，不是 Haven 旧名兼容层。
- 不改变 provider response mapping 或归一后的 `FinishReason`。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-llm`
- `cargo clippy --locked -p haven-llm -- -D warnings`

## 回滚与重置

回滚时可恢复冗余 attribute；它不改变接受的 JSON key、wire contract、数据或配置。
