# ADR 1346：将按需工具发现入口命名为 discover_tools

## 背景

模型可见入口 `tool_catalog` 同时支持目录搜索、查看具体工具 schema，以及加载选中的内置 operation。名称容易让模型将它理解为只读目录；Skill 与 MCP 又通过独立 loader 激活，入口职责不够直观。

## 决定

- 将 provider-facing 工具名从 `tool_catalog` 改为 `discover_tools`。`list`、`describe`、`load` 三种 action、输入输出 schema 和加载行为保持不变；Skill 与 MCP 继续分别使用 `load_skill`、`load_mcp`。
- 同步改名工具实现模块、类型、提示描述、安全映射、默认 UI 标签、Agent 提示、resume 加载选择识别、文档和测试中的对外工具名。
- `ToolCatalogPort`、`ToolCatalogSnapshot`、`tool_catalog_snapshot` 等内部目录接口继续使用 catalog 命名，因为它们表达目录数据与运行时快照，不是模型可调用入口。
- 不提供旧 provider 工具名别名，也不重写已保存的 transcript。旧配置段 `[tool_settings.tool_catalog]` 不自动迁移；用户如需保留该配置，需手动改为 `[tool_settings.discover_tools]`。旧会话无法恢复的内置加载选择可由新入口重新加载；如果 Provider 不接受包含旧工具名的历史调用，则从新会话继续。

## 替代方案

- 保留 `tool_catalog` 并只改描述：描述可说明能力，但工具名仍容易被理解为只读目录，拒绝。
- 改为 `tool_search`：无法覆盖精确 schema 查询和内置 operation 加载，拒绝。
- 拆成多个 provider-facing 工具：会扩大常驻工具面，并与现有 Skill/MCP loader 重叠，拒绝。

## 影响

Provider 每轮收到的新工具名为 `discover_tools`，其注册与安全元数据继续按 Safe 分类。数据库 schema、事件 payload 形状和 operation 输入输出不变；无需数据库重置。旧 per-tool 配置 key 与未完成 transcript 的边界按上方决定处理。

## 验证

- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 与 `git diff --check` 通过。
- 源码引用检查确认 provider-facing 入口使用 `discover_tools`；Rust 测试未运行。

## 回滚 / 重置

回滚代码与本文档即可恢复旧入口。回滚前若用户手动改过 `[tool_settings.discover_tools]`，需将其 key 改回 `[tool_settings.tool_catalog]`；数据库无需重置。
