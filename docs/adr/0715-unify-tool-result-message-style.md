# ADR 0715：统一 Tool result message 样式与名称

## 状态

已采纳并实施。

## 背景

Tool result renderer 将多种内容都命名为 `.tool-card-empty`：除空结果外还包括剪贴板写入成功、Shell 等待输出、ToolResultCard 的结果已显示与错误提示。多个 renderer 分别复制同一 label-medium 字号/行高、颜色和零外边距；Admin/Memory 使用小号字，Process 使用额外垂直间距，Media 沿用正文字号并只增加顶部间距，失败提示使用错误色和半粗体。

## 决定

- 各 Tool result 提示正文统一使用全局 `.tool-result-message`，其默认拥有零外边距、label-medium 字号/行高和 on-surface-variant 色。
- `.tool-result-message--compact`、`--spaced`、`--media` 与 `--error` 分别表达已存在的小号字、进程提示间距、媒体正文继承样式和错误色/字重差异。
- ToolResultCard 与专用 renderer 共享同一消息视觉规则；每个 renderer 仍拥有提示文案和显示条件。

## 替代方案

- 保留 `.tool-card-empty`：拒绝。该名错误覆盖成功、等待和错误消息。
- 每个 renderer 继续拥有相同基础规则：拒绝。排版相同，差异能用明确 modifier 表达。
- 合并结果 renderer：拒绝。提示正文只是共享 presentation，结果 shape 与状态仍由各 renderer 管理。

## 影响与验证

只改内部 class、基础样式 owner 与少量 modifier；文案、显示时机、字号/行高/颜色/间距和失败强调保持不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（698 records）与 `git diff --check`。Vitest 输出 `TimeoutNaNWarning` 后退出码为 0。

## 回滚

恢复 `.tool-card-empty` markup、各 renderer 的局部规则与测试 selector，删除全局 message 规则和 modifier，再撤销命名规范、路线图与本 ADR。无数据迁移。
