# ADR 1355：统一内置 operation 名称与输入 schema

## 状态

已采纳并实施 — 2026-10-21

## 背景

内置工具已经使用点号 operation view 和严格 JSON Schema，但存量名称混有 `object_verb`、父级词重复和查询名词化等形式，例如 `media.volume_get`、`haven.mcp.mcp_add`。这些名称还用作权限 capability、工具目录 stable name、提示词索引、renderer、配置键及持久化任务目标；只改 catalog 会留下不可调用的配置或授权。

schema 同样已有精确操作分支和运行时边界，但项目缺少明确的共同基线，容易把 schema 指引误当成执行校验，或把异构工具参数强行包装成相同 shape。

## 决定

1. 按[命名规范](../naming.md#模型可见内置工具名与输入-schema)统一新名称：独立入口保留简短名；操作 view 使用能力路径和动词优先的动作段；复合动作段使用 snake_case；父级已表达领域时不重复前缀。稳定名唯一，旧名不注册为可执行别名。
2. 将现有不符合该规则的模型可见名称批量迁移：

| 旧名称 | 当前名称 |
|---|---|
| `files.outline` | `files.get_outline` |
| `files.summary` | `files.summarize` |
| `files.create_dir` | `files.create_directory` |
| `lsp.definition` | `lsp.find_definition` |
| `lsp.implementation` | `lsp.find_implementations` |
| `lsp.type_definition` | `lsp.find_type_definition` |
| `lsp.references` | `lsp.find_references` |
| `lsp.hover` | `lsp.get_hover` |
| `lsp.document_symbols` | `lsp.list_document_symbols` |
| `lsp.workspace_symbols` | `lsp.search_workspace_symbols` |
| `lsp.diagnostics` | `lsp.get_diagnostics` |
| `clipboard.history` | `clipboard.list_history` |
| `window.foreground` | `window.get_foreground` |
| `window.screenshot` | `window.capture_screenshot` |
| `window.ui_tree` | `window.get_ui_tree` |
| `media.ocr` | `media.run_ocr` |
| `media.volume_get` | `media.get_volume` |
| `media.volume_set` | `media.set_volume` |
| `media.mute_get` | `media.get_mute` |
| `media.mute_set` | `media.set_mute` |
| `agent.children` | `agent.list_children` |
| `agent.history` | `agent.read_history` |
| `agent.inbox` | `agent.list_inbox` |
| `agent.ack` | `agent.acknowledge` |
| `agent.profile` | `agent.get_profile` |
| `agent.status` | `agent.get_status` |
| `session.links` | `session.list_links` |
| `session.history_search` | `session.search_history` |
| `session.history_read` | `session.read_history` |
| `session.linked_history_search` | `session.search_linked_history` |
| `session.linked_history_read` | `session.read_linked_history` |
| `session.asset_search` | `session.search_assets` |
| `system.info` | `system.get_info` |
| `system.display` | `system.list_displays` |
| `system.power.status` | `system.power.get_status` |
| `haven.diagnostics.status` | `haven.diagnostics.get_status` |
| `haven.diagnostics.logs_tail` | `haven.diagnostics.tail_logs` |
| `haven.diagnostics.sessions` | `haven.diagnostics.list_sessions` |
| `haven.diagnostics.errors` | `haven.diagnostics.list_errors` |
| `haven.config.config_get` | `haven.config.get` |
| `haven.config.logs_level` | `haven.config.set_log_level` |
| `haven.skills.skills_list` | `haven.skills.list` |
| `haven.skills.skill_enable` | `haven.skills.enable` |
| `haven.skills.skill_disable` | `haven.skills.disable` |
| `haven.skills.skill_create` | `haven.skills.create` |
| `haven.tools.tool_enable` | `haven.tools.enable` |
| `haven.tools.tool_disable` | `haven.tools.disable` |
| `haven.mcp.mcp_list` | `haven.mcp.list` |
| `haven.mcp.mcp_connect` | `haven.mcp.connect` |
| `haven.mcp.mcp_disconnect` | `haven.mcp.disconnect` |
| `haven.mcp.mcp_add` | `haven.mcp.add` |
| `haven.mcp.mcp_update` | `haven.mcp.update` |
| `haven.mcp.mcp_toggle` | `haven.mcp.toggle` |
| `haven.mcp.mcp_remove` | `haven.mcp.remove` |
| `haven.mcp.mcp_reload` | `haven.mcp.reload` |
| `haven.mcp.mcp_reconnect` | `haven.mcp.reconnect` |
| `haven.mcp.mcp_refresh` | `haven.mcp.refresh` |

3. `haven-common::tools::BUILTIN_TOOL_NAME_RENAMES` 是这批历史名称的迁移表。配置加载时将永久权限和 per-operation tool settings 原子重写为当前名称；权限键冲突时 deny 胜出，settings 冲突时当前名称项优先，disabled 状态合并为 disabled。
4. 当前数据库 schema 的 `init_schema` 执行带 kv marker 的同 schema 值迁移，将 session authorization grants、Schedule target 与 ToolRun target 改为当前名称。冲突授权 deny 胜出，并保留更窄的 permission target。该变更不改表结构或 `SCHEMA_VERSION`，也不重新引入旧版本 schema 的运行时升级。
5. `session_events` 是 append-only 历史，已有事件中的 tool name 不改写。它保留事件发生时的名称；新模型目录、权限和新执行目标只使用当前名称。provider/MCP 对端工具名保持原样。
6. 内置输入 schema 采用统一基线：模型可见操作是封闭 object；每个属性、必填关系、缺省/null 语义和范围明确；聚合 schema 用 `oneOf` / `const` 绑定内部 operation 分支；Haven ID schema 复用 canonical helper。schema 由能力 owner 维护，runtime 仍执行类型、业务、权限及动态边界检查，provider dialect 的转换只在 LLM adapter 发生。动态结果和外部 MCP schema 不强行变成统一业务 DTO。

## 替代方案

- 保留旧名并为每个旧名注册执行别名：拒绝，会继续暴露两套模型调用与权限身份。
- 删除全部持久数据并让用户重建：拒绝，配置、会话授权、定时任务和 ToolRun 目标可以原值迁移，无需破坏性重置。
- 将所有工具 schema 改成同一个参数 envelope：拒绝，操作参数本来就异构；共同约束应落在对象封闭、字段语义和运行时校验上。

## 影响与验证

当前 operation name 已同步到工具目录、权限策略、安全确认摘要、提示词、renderer、调用方和测试。永久权限与 per-operation settings 在配置读取时迁移；会话授权、Schedule 和 ToolRun 的持久目标在现有 schema 下迁移。历史事件保留原始名称。数据库结构与配置字段 shape 不变。

验证结果：

- `cargo fmt --all -- --check`、`cargo test --workspace --locked`、`cargo check --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings` 均通过。
- `corepack pnpm run check` 通过；工具结果、身份、manifest 与展示相关的 4 个 UI 测试文件共 134 项通过。
- 全量 UI 测试未能通过：低并发重跑报告 6 个 Session 相关失败，随后长时间无新输出并被中止。因此全量 UI suite 在当前工作区仍未验证为通过。

## 回滚

恢复旧工具名、权限 key 与 renderer 前，应反向迁移配置及 Schedule/ToolRun/session grant 值，并还原本 ADR 的 operation-name 引用；已写入当前名称的数据库值不应被旧二进制直接解释。`session_events` 的历史值始终保留原样。无 schema 版本变化，不需要删除或重建数据库。
