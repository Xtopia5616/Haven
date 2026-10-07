# ADR 0709：共用 Session welcome 展示组件

## 状态

已采纳并实施。

## 背景

`SessionEmptyState` 是 `SessionTimeline` 在首屏无活动内容时的轻量 welcome；`SessionMessageTimeline` 在 timeline projection 为空时又复制了相同的 logo、文案、hotkey hints 与 7 组 CSS。两个调用状态不同，但展示完全相同；后者单独多一个 y=12、330ms 入场动画。`SessionTimeline` 还单独拥有 loading 与 run-end 状态优先级。

## 决定

- 删除 `SessionEmptyState`，创建唯一展示组件 `SessionWelcome`，由首屏空态和空 timeline projection 共用 markup 与 CSS。
- `SessionWelcome.animated` 显式选择入场动画；默认关闭以保留首屏轻量展示，timeline projection 分支传 `animated` 以保留 330ms 动画。
- `SessionTimeline` 继续拥有 loading/run-end gate；`SessionMessageTimeline` 继续拥有 Session message、activity 和 ToolRun timeline 投影。

## 替代方案

- 仅把 CSS 提到全局并保留两份 markup：拒绝。内容、logo 与交互提示也完全重复，继续各自修改会造成漂移。
- 把 SessionTimeline 与 SessionMessageTimeline 合成一个组件：拒绝。外层加载/终态决策与内层消息/活动列表有不同生命周期与状态 owner。

## 影响与验证

仅合并 Session 欢迎视图；首屏布局、文案、hotkeyBinding 和空 timeline 的入场动画保持不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（692 records）和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复 `SessionEmptyState` 与 `SessionMessageTimeline` 内 welcome markup/styles，撤销两个调用点、测试样式 fixture、命名规范和路线图变更。无数据迁移。
