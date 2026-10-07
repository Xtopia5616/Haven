# ADR 0706：共用 Tool result label 样式

## 状态

已采纳并实施。

## 背景

12 个 Tool result renderer 各自声明了相同的小号字体、半粗体、行高、变体文字颜色和底间距。该 class 原名 `.tool-card-count`，但实际内容除了数量，还包括“用户信息”“收到回复”和“输出过长已截断”等 section label，因此原名不能准确说明用途。

## 决定

- 所有 renderer 的该类 section label 和计数改用 `.tool-result-label`。
- 共享文字样式由 `ui/src/app.css` 唯一拥有；从各 Svelte component 删除重复声明。
- `ToolResultCard` 的卡片 shell 与各 result renderer 的正文 section label 保持不同 owner；当前正文 label 样式只作用于 `.tool-result-label`。

## 替代方案

- 保留 `.tool-card-count`：拒绝。该 class 经常承载非计数的分区标题和状态标签。
- 让每个 renderer 保留本地样式：拒绝。检查到的 12 份声明完全相同，没有领域差异需要单独 owner。

## 影响与验证

仅改名内部 class 并把相同 CSS 规则提升到全局样式表；字号、颜色、字重、间距和布局保持不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（689 records）和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复各 renderer 的旧 class/style 声明，并撤销全局样式、测试 selector、命名规范与路线图变更。无数据迁移。
