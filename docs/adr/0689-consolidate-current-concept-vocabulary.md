# ADR 0689：收敛当前概念词表与架构文档

## 状态

已采纳并实施。

## 背景

全仓术语审查发现，`docs/architecture.md` 的 Agent 部分有两条重复的 ReAct 架构描述，仍引用已删除的 `retries` 模块和泛名 `RunEngine`，且没有列出 `identity`、`sidecars`、`metrics`、`tool_ports` 等当前模块。相邻 ToolRun 段落仍以旧 Job 用语描述当前 ToolRun 实体，并把既有 `toolRunStore` 描述成未统一的生命周期 reducer。跨层词表也没有集中解释 `SessionEvent`、持久 `Message`、`TranscriptRecord`/`TranscriptProjection`、interaction event 投影，以及 ToolDef、ToolManifest、ToolHandle 和 catalog 的区别。

## 决定

- 删除重复 ReAct 描述，依据当前模块和 owner 收敛为一条；使用 `SessionRunEngine`、`response_policy`、ToolRun 等当前名称，并明确 shared activity-card 字段与 kind-specific execution details 的边界。
- 在 `docs/naming.md` 的领域术语表集中说明 durable event、materialized message、transcript projection、process-local event、interaction routing/UI projection，以及 Tool implementation/definition/manifest/catalog 的角色。
- 更新跨层输出清单中已过期的 `StoredBranchPoint`、`ToolBox`、`ToolsManager` 和 outbox tuple 说明；未决的 API、renderer shape、配置字段和 IPC/event 审计继续留在路线图。

## 替代方案

- 只在 ADR 增加术语说明：拒绝。当前规范和架构文档是后续实现者的权威来源，必须在对应文档中直接修正。
- 将名称相近的 event、transcript、tool catalog 类型强制合为一个类型：拒绝。它们的持久性、生命周期、消费边界和权限含义不同；词表应说明边界，不以相似字段合并 owner。

## 影响与验证

- 只更新架构、命名和契约清单文档，不改变代码行为、配置、数据库、Tauri/IPC 或用户界面。
- 验证：Markdown 差异与链接检查、`git diff --check`。

## 回滚

恢复本 ADR 前的文档内容即可，无数据或运行时回滚。
