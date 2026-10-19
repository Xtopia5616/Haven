# ADR 1329：按用户任务重分类内置工具目录

## 状态

Accepted — 2026-10-19

## 背景

ADR 0141 将全部本机能力归入 `system`。随着文件、桌面自动化、媒体、网络搜索和 Shell 能力增长，设置页与 Agent 工具目录都把不同任务放在同一大类中，难以浏览；`system` 的路由摘要也不再能准确描述组内工具。

## 决定

保留 `ToolCatalogGroup` 作为 Agent prompt、工具目录和设置页共用的展示/路由元数据，并按用户任务拆分内置能力：

| 分类 | 工具 |
|---|---|
| `haven` | `ask`、Memory、ToolRun、Schedule 和 Haven 管理操作 |
| `agent` | `agent.*` 与跨会话 `session.*` 协作操作 |
| `files` | `files.*` 文件与工作区操作 |
| `desktop` | `clipboard.*`、`input.*`、`window.*` |
| `system` | `system.*` 与 `notify` |
| `media` | `media.*` 媒体、语音和音量操作 |
| `network` | `http` 与 `web_search_api` |
| `shell` | `shell` 命令执行 |
| `skills` / `mcp` | 保留给用户配置的外部来源 |
| `other` | 未分类扩展工具的兜底值 |

每个分类都提供同一后端枚举上的路由摘要。UI 使用对应的中文分类名；分类继续按能力族、根能力和具体操作三级显示，启停仍按原操作进行。

分类是展示和目录提示元数据，不表达授权或执行边界。工具名、权限 key、风险、确认要求、参数 Schema、执行策略及 provider-facing 工具定义保持不变。

## 替代方案

- 只在 UI 按工具名推导分类：拒绝。会重新引入 ADR 0141 消除的重复分类来源，并使 Agent 工具目录与设置页不一致。
- 继续使用 `system` 并只调整排序：拒绝。不能解决同一分类混合多个不同用户任务的问题。

## 影响与验证

`ToolCatalogGroup` 新增 `files`、`desktop`、`media`、`network` 和 `shell` wire 值，须同步生成 Rust→TypeScript IPC 响应 shape。没有数据库、配置或 durable event 变化；新旧客户端不能混用分类枚举，因此 Rust 和 UI 应同版本发布。现存设置不需要迁移或重置。

验证覆盖各组摘要、代表性内置操作分类、模型工具目录分组、UI 分类顺序与显示标签、生成 IPC 契约漂移。回滚只需恢复旧分类并重新生成前端契约，不涉及持久数据。
