# ADR 0972：用 floor 表达响应 token 下界

## 状态

已采纳并实施。

## 背景

`ContextLimitsConfig.max_response_tokens` 的值会在构造 router 时用于 `max(endpoint.max_tokens, value)`。它抬高较小 endpoint 的输出 token 数，保留高于该值的模型专属配置；随后 router 还会按上下文窗口做有效上限裁剪。因此该配置是全局下界，名称却容易被理解为最大输出硬上限。设置页原文已将其称为“回复输出 token 下限”。

## 决定

1. 持久配置字段与 generated settings contract 改名为 `response_token_floor`；`LlmConfig::materialize` 和 `RouterConfig::apply_caps` 的对应参数也用 `response_token_floor`。
2. 保持计算行为不变：较小的 endpoint `max_tokens` 提高到 floor，较大的值保留；router 继续将最终值限制在 provider 可接受的 context window 内。
3. 不为旧 TOML key `max_response_tokens` 添加 alias。配置加载会按现有策略备份原文件并使用默认配置；用户可手动改为 `response_token_floor` 或仅重建 `config.toml`。数据库不变。

## 替代方案

- 将设置解释为硬上限并改成 `min(endpoint.max_tokens, value)`：拒绝；这会改变现有输出预算行为，且与设置页现有说明相反。
- 保留 `max_response_tokens` 并只追加注释：拒绝；字段名继续表达相反的约束方向。
- 叫 `min_response_tokens`：暂缓；该字段的操作对象是 endpoint 的输出 token 预算，而非要求模型生成的最少 token 数；`response_token_floor` 更准确表达被抬高的预算下界。

## 影响与验证

- 配置结构、TOML 与 Settings IPC 字段统一表达下界；设置页已有的“下限”文案继续适用。
- 配置 round-trip 覆盖当前字段，旧字段有拒绝测试；UI 保存测试检查设置回环值；现有 materialize 测试覆盖低值上调和高值保留。
- 不改变路由计算、数据库或持久事件，不需要数据库重置。

## 回滚

恢复旧 Rust/config/UI/generated contract 字段名并同步撤销测试、命名与 reset 文档。已保存的 `response_token_floor` 配置需在回滚前改回 `max_response_tokens`；数据库无需处理。
