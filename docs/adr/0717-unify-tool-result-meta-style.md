# ADR 0717：统一 Tool result 元信息类与样式 owner

## 状态

已采纳并实施。

## 背景

14 个 Tool result renderer 将结果元信息统一标为 `.tool-card-meta`，名称把内容错误归属到外层卡片。Agent、Clipboard、File、HTTP、Runs、Schedule、System 与 WebSearch 的最终 compact 排版完全一致：label-small 字号/行高、on-surface-variant 色和 2xs 顶间距。Clipboard、Runs、Schedule 还保留 medium 字号/行高规则，随后又被同一选择器覆盖为 small。

其余 renderer 有真实排版差异：Input 只需 small 字号与颜色；结果概览和行内按钮信息曾分别由 `.tool-card-meta`、`.input-meta` 声明这套样式，现统一用 `--input`。Media 只需颜色和 xs 顶间距；Memory 使用 small 字号/行高、颜色和 xs 底间距；Shell 使用 medium 字号/行高、颜色和 2xs 顶间距。Process 与 Window 没有匹配的局部 CSS。

## 决定

- 所有结果元信息统一使用 `.tool-result-meta`，不再以卡片作为样式归属。
- compact、input、media、memory、shell 五种现有视觉差异分别由 `.tool-result-meta--compact`、`--input`、`--media`、`--memory`、`--shell` 表达，样式集中由 `ui/src/app.css` 拥有。
- Process 与 Window 保留无 modifier 的基础语义标记，不添加此前不存在的视觉样式。
- 删除被覆盖的 medium 规则和组件内重复样式；结果内容与布局保持原状。

## 替代方案

- 保留 `.tool-card-meta`：拒绝。元信息位于 Tool result 内容层，外层卡片不是该内容的样式 owner。
- 把五种排版强行压成一种：拒绝。字号和间距差异对应现有结果布局，不能仅因名称相同而视为相同样式。
- 每个 renderer 继续维护共同的 compact 规则：拒绝。八个 renderer 的最终值相同，且全局已有 Tool result presentation 样式 owner。

## 影响与验证

仅调整 UI 内部 class 与 CSS owner。保留 compact、input、media、memory、shell 已有的字号、行高、颜色和间距；Process、Window 不新增视觉效果。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests）、ADR index（700 条）与 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，但进程成功退出且测试全部通过。

## 回滚

恢复各 renderer 的 `.tool-card-meta` markup 和局部样式，移除全局 modifier，再撤销命名规范、路线图与本 ADR。无数据迁移。
