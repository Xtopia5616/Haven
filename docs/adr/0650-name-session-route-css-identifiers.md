# ADR 0650：统一 Chat route 的 Session CSS 标识

## 状态

已采纳并实施。

## 背景

Chat route 的主内容容器已经承载当前 Session UI，侧栏组件名也是 `SessionRail`，但该容器 selector 叫 `.conversation-column`，rail 的退出 keyframe 叫 `conversation-rail-exit`。这些是 Haven 内部 CSS 标识，指向的是 Session page layout 与 SessionRail 动画。

## 决定

1. 主内容容器 class 改为 `.session-column`。
2. SessionRail 的退出 keyframe 改为 `session-rail-exit`，并同步动画引用。
3. conversation 自然语言描述与外部协议字段不在此范围。

## 替代方案

- 保留旧 class/keyframe：拒绝。它们只在同一个 route 文件内被定义和消费，名称不再符合项目 Session 实体术语。
- 批量替换所有 CSS 中的 conversation：拒绝。审计只发现这两个内部 selector/keyframe，且其他自然语言或协议语义应保留。

## 影响与验证

只重命名 Svelte route 文件中的内部 class 与 keyframe，DOM 布局、动画数值、状态和行为不变；无持久化或 wire 变化。验证 UI check、UI tests、生产 build 与 ADR index。

## 回滚

恢复原 selector/keyframe 名称并同步撤回命名规范、路线图和 ADR 索引；无需状态回滚。
