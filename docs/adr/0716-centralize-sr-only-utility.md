# ADR 0716：集中屏幕阅读器隐藏文本样式

## 状态

已采纳并实施。

## 背景

`InputRouter`、`ToolRunCenter`、`MemoryRecall` 与 `ToolsView` 各自复制相同的 `.sr-only` 规则：将有语义的 label 移出可视布局，同时保留给辅助技术读取。该样式属于共享无障碍工具，不依赖组件局部布局。

## 决定

- 将 `.sr-only` 的完整规则集中到 `ui/src/app.css`。
- 四个组件保留现有 class 和语义 label，删除局部重复规则。

## 替代方案

- 保留每个组件的局部规则：拒绝。四份声明完全相同，且 utility 不依赖组件样式 owner。
- 合并输入与搜索组件：拒绝。输入和搜索 label 的内容与行为不同，共享的只有 accessibility utility。

## 影响与验证

仅合并内部 CSS owner；可见布局与辅助技术可读文本不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests）、ADR index（699 条）与 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，但进程成功退出且测试全部通过。

## 回滚

将 `.sr-only` 规则恢复到四个组件并删除全局规则，撤销命名规范、路线图和本 ADR。无数据迁移。
