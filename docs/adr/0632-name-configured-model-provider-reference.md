# ADR 0632：区分模型的连接引用与供应商身份

## 状态

已采纳并实施。

## 背景

`ModelConfig.provider` 保存模型所绑定的 `ProviderConfig.name`，也就是用户配置的连接名称；`discover_models` 的 `provider` 参数也传入该连接名称。`ProviderConfig.provider` 和物化后的 `ModelEndpoint.provider` 保存 OpenAI、Ollama 等供应商身份。相邻字段都叫 `provider`，调用方必须依赖所在结构和注释才能分辨“选哪个连接”与“连接属于哪家供应商”。设置界面也把生成的模型 wire shape 直接作为编辑草稿，沿用同一歧义。

## 决定

1. Rust 模型配置字段改为 `ModelConfig.provider_name`；Serde、TOML、JSON 与 generated IPC contract 均使用 `provider_name`。
2. 按连接名查找的 `LlmConfig::provider` 改为 `provider_config_by_name`。
3. 设置编辑器的 `ModelDraft` 与 chat model option 使用 `providerName`；加载与保存时在 UI 边界映射到 generated contract 的 snake_case `provider_name` 字段。
4. `discover_models` 的连接名参数使用 Rust `provider_name` 与 UI `providerName`，IPC 边界遵循 snake_case↔camelCase 约定。
5. `ProviderConfig.provider` 与 `ModelEndpoint.provider` 继续表示供应商身份；本 ADR 不对这类实际供应商字段做批量替换。
6. 旧的 `llm.models[].provider` 不再接受。配置解析失败时按现有规则备份原配置并使用默认配置；用户需按 `docs/release-and-reset.md` 仅重建配置，无需删除数据库。

## 替代方案

- 保留旧 TOML/IPC key 并仅改内部字段：拒绝，用户已明确本次命名收敛不考虑向下兼容；保留旧歧义 key 会让配置与 generated contract 继续暴露原问题。
- 把供应商身份改名为 `vendor`：暂缓，`ModelEndpoint` 与 LLM adapter、diagnostics、日志及 capability 检查广泛使用该概念，需作为单独的跨 crate 词汇审计处理。
- 仅增加注释而保留内部 `provider` 字段：拒绝，Rust 和 UI 调用点仍会混淆连接引用与供应商身份。

## 影响与验证

- Rust `ModelConfig` 字段、持久配置键、IPC 字段和设置编辑器用名均明确区分连接名称与供应商身份。
- 设置 UI 内部草稿与 chat toolbar option 使用 `providerName`，只在 generated IPC 边界映射为 snake_case `provider_name`。
- Rust 测试检查新 JSON key 往返并拒绝旧字段；UI 测试检查 wire→draft→input 映射；配置重置说明记录在 `docs/release-and-reset.md`。
- 全项目领域术语与架构角色审计仍在进行；本切片不表示配置或 UI 命名域已审计完成。

## 回滚

恢复 Rust 字段与方法名及 UI 草稿字段名，并同步撤销 mapper 与测试；配置应随回滚版本重新创建，数据库无需迁移或重置。
