# ADR 0713：共用 ExpandableContextCard 元信息行排版

## 状态

已采纳并实施。

## 背景

Builtin Tool family、Tool root、MCP server 和 Skill 卡片都向 `ExpandableContextCard` 提供 header metadata 行。四者分别复制相同的 flex、对齐、间距、label-small 字号/行高和换行规则，局部类名 `.card-meta` 没有标明它属于哪类 card。现有全局 `.workspace-item-card-meta` 用于其他列表卡片的底部信息，包含 `margin-top: auto` 与首尾子项布局约束，不适用于这些可展开卡片 header。

## 决定

- 四种卡片统一使用 `.expandable-context-card-meta`，由 `ui/src/app.css` 唯一拥有基础排版。
- 各 renderer 继续拥有实际 metadata 内容及其具体徽标、状态、transport、version 和 endpoint 样式。
- `.workspace-item-card-meta` 继续用于 workspace 列表卡片的底部元信息，不与 header 行合并。

## 替代方案

- 复用 `.workspace-item-card-meta`：拒绝。它包含不同的垂直布局与子项截断/固定宽度约束。
- 保留四份局部 `.card-meta`：拒绝。行的 presentation 完全相同，且名称无法定位样式 owner。
- 合并四种卡片 renderer：拒绝。header 内容、生命周期交互与卡片操作仍属于各自领域。

## 影响与验证

只统一内部 class 和 CSS owner，header 数据、徽标、卡片交互与布局数值保持不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（696 records）与 `git diff --check`。Vitest 输出 `TimeoutNaNWarning` 后退出码为 0。Prettier 检查指出 `SkillCard.svelte` 存在格式差异；对 HEAD 版本运行同一检查也失败，因此本切片不重排无关存量代码。

## 回滚

还原四种卡片的 `.card-meta` markup 与局部 CSS，删除全局规则并撤销命名规范、路线图和本 ADR。无数据迁移。
