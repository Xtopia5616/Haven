# ADR 0092：严格 OpenAI-compatible provider 的工具 schema 投影

- 状态：Accepted
- 日期：2026-09-07

## 背景

xAI/Grok 的工具 schema 校验拒绝根级 `anyOf`、`oneOf` 或 `allOf`，即使 schema
同时声明了 `type: object`。Haven 最近把 `schedule` 以及多个聚合内置工具收紧为
按操作分支的根级 `oneOf`，因此请求会在 ReAct 执行前收到 provider 的 400。

## 决定

1. `haven-tools` 保留完整的操作分支 schema，继续作为工具调用的本地权威校验；不
   为了 provider 兼容而削弱执行边界。
2. `haven-llm` 的 xAI chat adapter 在发送工具前，把根级 union 投影为普通 object：
   合并分支属性、保留所有分支共同必填字段，并合并 discriminator 的 `const`/`enum`。
   即使配置显式选择 `openai-chat`，只要 provider 是 xAI/Grok 或 endpoint 主机是
   `api.x.ai`，仍启用该投影。
3. 该投影只作用于 xAI wire 请求；普通 OpenAI-compatible chat、OpenAI Responses、
   Anthropic 和 Gemini 保留原始 schema。对象属性内部的 union 不做改写。
4. Prompt cache key 使用与实际请求相同的 provider 投影，避免 schema 与缓存路由键
   不一致。

## 替代方案

- 直接删除所有工具的根级 `oneOf`：拒绝，这会削弱本地契约并把 provider 特例扩散到
  `haven-tools`。
- 只给 `schedule` 的内部 union 分支补 `type: object`：拒绝，其他内置工具和动态
  MCP/Skill schema 仍可能触发同类 provider 限制。
- 对所有 provider 统一展平：拒绝，OpenAI 等 provider 能够消费更精确的原始 schema。

## 影响与验证

模型在 xAI 上看到的是可接受的 object-root schema；执行前仍由 Haven 的原始 schema
检查操作级必填字段和互斥字段。普通 OpenAI adapter 的 schema 行为不变。

验证包括 LLM 单元测试、工具 schema 回归测试、格式化、workspace check、Clippy、
workspace 测试和 UI 门禁。

## 回滚

回退对应代码提交即可；本变更不修改数据库、配置、快照或持久化工具数据。
