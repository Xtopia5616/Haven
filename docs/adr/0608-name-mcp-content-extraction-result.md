# ADR 0608：命名 MCP 内容提取结果

## 状态

已采纳并实施。

## 背景

`extract_mcp_content` 把 MCP `tools/call` 的 content blocks 转成两个稳定值：供 UI/tool result 渲染的结构化 JSON output，以及包含可读文本和媒体摘要的 Agent 文本 summary。原返回 `(Value, String)`，客户端与测试均按位置解构；结构化结果与摘要在同一提取过程中生成，但调用角色不同。

## 决定

1. 使用 `McpContentExtraction { output, text_summary }` 表达提取结果。
2. MCP client 按字段把结构化 output 和 error summary 映射到现有 `McpCallOutput`。
3. 测试按字段断言两个结果，不再依赖 tuple 顺序。

## 替代方案

- 保留 tuple 并只在调用点命名局部变量：拒绝，位置语义仍是函数 API 的一部分，新增调用方仍需从实现推断返回顺序。
- 合并到 `McpCallOutput`：拒绝，内容提取发生在 MCP client 成功/错误状态适配之前；提取结果不是一次工具调用的最终 success/error envelope。

## 影响与验证

- 仅改变 `haven-mcp` 内部 Rust API；结构化 JSON 字段、文本内容、payload 上限和 MCP protocol 均不变。
- 命名路线图 §5.7 继续保持 Active；其他 crate 返回值、UI 命名和跨层契约仍待继续逐域审计。
- 验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-mcp`、`cargo clippy --locked -p haven-mcp -- -D warnings`、`cargo test --locked -p haven-mcp`、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(Value, String)` 返回值与客户端/测试 tuple 解构；无需数据库、配置或 wire 迁移。
