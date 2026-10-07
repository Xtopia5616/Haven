# ADR 0704：删除重复的 Tool result 列表 wrapper

## 状态

已采纳并实施。

## 背景

`ToolResultList` 分页结果并提供“显示更多”；`ToolCardList` 没有分页，也不负责创建卡片，只输出一个带 `.tool-card-list` 全局样式的 `div`。Agent、Process 和 ToolRun renderer 用该组件包住分页内容；其他 renderers 则直接写同样的 `div`，组件没有独立状态、约束或行为。此外，组件内又复制了一份与全局规则相同的滚动样式。

## 决定

- 删除无逻辑 `ToolCardList` 组件与只验证静态 wrapper 存在的组件测试；三个调用点改为直接写展示 `div`。
- 将全局样式 class 从 `.tool-card-list` 改为 `.tool-result-scroll-area`，现有直接使用点同步迁移；滚动样式只由 `ui/src/app.css` 拥有。
- `ToolResultList` 继续负责分页与显示更多；滚动 surface 是可选展示样式，两者不合并为组件。

## 替代方案

- 把滚动逻辑并入 `ToolResultList`：拒绝。分页和滚动是独立展示职责，部分 renderer 会分页但不需要限高滚动。
- 单独保留 `ToolResultScrollArea` 组件：拒绝。它仍然只是输出一个 div 和共享 class，没有复用超出普通标记的行为。

## 影响与验证

删除只包装同一展示 class 的 UI 内部组件；DOM wrapper 与分页/滚动行为不变。全局样式 class 仅是内部呈现名，不影响 IPC、数据库或配置，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（687 records）和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复已删除的组件 wrapper，撤销全局 class 与调用点更新，并恢复命名规范与路线图条目。无数据迁移。
