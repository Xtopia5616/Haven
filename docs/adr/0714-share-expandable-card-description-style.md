# ADR 0714：共用 ExpandableContextCard 正文说明样式

## 状态

已采纳并实施。

## 背景

Builtin Tool family 与 Skill 卡片在 `ExpandableContextCard` 的 body 中各自声明 `.desc`，两个规则使用相同的 body-small 字号、on-surface-variant 颜色、上下 margin 和行高。局部类名过于宽泛，没有说明这是可展开卡片中的正文描述。

## 决定

- 两个 renderer 统一使用 `.expandable-context-card-description`，基础字体、颜色、间距和行高由 `ui/src/app.css` 唯一拥有。
- 描述文本与回退文案继续由 Builtin Tool 和 Skill renderer 决定。

## 替代方案

- 保留两份 `.desc`：拒绝。两处样式逐项相同且具有相同的 body description 角色。
- 合并两个卡片 renderer：拒绝。正文数据来源与卡片行为不同，共享范围仅是说明段落排版。

## 影响与验证

只统一内部 class 与样式 owner；文案、卡片交互和视觉属性不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（697 records）与 `git diff --check`。Vitest 输出 `TimeoutNaNWarning` 后退出码为 0。

## 回滚

恢复两个组件本地 `.desc` class 与样式，删除全局规则并撤销命名规范、路线图和本 ADR。无数据迁移。
