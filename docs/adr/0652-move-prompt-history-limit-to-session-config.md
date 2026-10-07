# ADR 0652：将 prompt-history 限制配置归入 Session

## 状态

已采纳并实施；它对 ADR 0560 决定 4 的配置 key 保留部分作出后续修订。

## 背景

ADR 0560 将 agent 首次 system prompt 需要加载的历史条数 `session_window_size` 保留在 `MemoryConfig`，理由是配置 key 已持久化。实际消费者是 App 装配中的 `AgentLayer::session_prompt_history_limit`；它为 fresh-run 的首次 system prompt 附加最近 session messages，与 Memory 的事实提取配置不是同一职责。设置界面也把它展示为 Memory 的“近期消息窗口”，没有准确描述该值的生效范围。用户已明确本次不需要向下兼容，也要求重新处理此前保留的兼容项。

## 决定

1. 从 `MemoryConfig` 移除 `session_window_size`，在 `SessionConfig` 增加 `prompt_history_limit`，TOML 路径为 `[session].prompt_history_limit`，默认值保持 50。
2. App 启动从 `cfg.session.prompt_history_limit` 装配 Agent 的 `session_prompt_history_limit`；Generated settings/IPC 类型随 Rust config DTO 更新。
3. 设置 UI 将控件归入“会话与执行”，说明其控制新会话首次 system prompt 的历史消息条数；Memory 区域只管理 Memory 功能。
4. 不为 `[memory].session_window_size` 添加 alias 或迁移。由于 `MemoryConfig` 拒绝未知字段，包含旧 key 的配置会按现有规则备份原件并以默认配置启动；用户可手动改到新路径或删除 `config.toml` 重建。
5. 数据库 schema、会话 transcript、读取条数语义与顺序均不变；无需删除数据库。

## 替代方案

- 只把旧字段改名但继续放在 MemoryConfig：拒绝。字段名变清楚仍会让 Memory 持有 Agent Session prompt 策略。
- 保留旧 TOML key 或静默迁移：拒绝。测试版本不需为旧配置保留兼容解析；配置 loader 已提供原文件备份与默认值恢复。
- 把它放入泛化的 ContextLimitsConfig：拒绝。该配置拥有 token、字符与 payload 预算；本字段限制的是 Session prompt 输入消息数量。

## 影响与验证

这是 Rust 配置结构、TOML key、生成 IPC settings DTO、App 装配与 renderer Settings contract 的破坏性变更。新增当前 key 的正向 round-trip 和旧 key 拒绝测试。运行 IPC generation/drift checks、Rust workspace fmt/check/strict Clippy/tests、UI check/tests/build、ADR index 与 staged diff checks。发布/reset 文档说明只需重建或手动修正 config.toml，不需重置数据库。

## 回滚

撤回 SessionConfig 新字段并恢复 MemoryConfig 字段与 UI 映射，重新生成 IPC types，并同步回滚 release/reset 文档和本 ADR 索引；无需数据库或会话数据回滚。
