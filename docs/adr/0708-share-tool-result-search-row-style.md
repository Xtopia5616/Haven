# ADR 0708：共用 Tool result 搜索行样式

## 状态

已采纳并实施。

## 背景

Clipboard、FileSearch 与 WebSearch renderer 分别复制了相同的 search row、路径链接及 hover 和次级文本 CSS。它们的字段内容不同，但行的对齐、间距、路径截断和次级信息显示完全相同。File/Web 的路径链接由 `ExternalRef` 输出，旧样式因此还要在各组件中重复写 `:global(...)`。

## 决定

- 统一使用 `.tool-result-search-row`、`.tool-result-search-path` 和 `.tool-result-search-detail`。
- 共用展示规则迁入 `ui/src/app.css`；移除三个 renderer 内重复规则和 `:global` 覆盖。
- renderer 继续拥有各自数据映射、path/URL、行号、标题和剪贴板内容。

## 替代方案

- 新建共享 row component：拒绝。剪贴板、文件和网页的子节点差异明显，唯一重复的是 CSS presentation。
- 把结果字段统一成一个 DTO：拒绝。数据语义不同，当前只有视觉规则相同，不存在可合并的结果契约。

## 影响与验证

仅改名内部 class 并统一视觉规则；行内容、striping、宽度、截断和 hover 行为不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（691 records）和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复各 renderer 的旧 class 与本地 CSS，撤销全局样式、测试 selector、命名规范和路线图变更。无数据迁移。
