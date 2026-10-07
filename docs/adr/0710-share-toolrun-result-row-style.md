# ADR 0710：共用 ToolRun 结果行基础样式

## 状态

已采纳并实施。

## 背景

`ToolRunsResult` 和 `ToolScheduleResult` 都展示 ToolRun 状态行。两者复制了完全相同的 flex 对齐、gap、字体大小和行高；ToolRun ID 也分别重复声明 mono 字体、code size、line-height 和颜色。后台结果列表还需要对长 ID 省略截断，而定时操作的单个短 ID 当前不截断。

## 决定

- `.tool-run-row` 的行布局由 `ui/src/app.css` 唯一拥有。
- `.tool-run-id` 的字体与基础颜色也由全局样式拥有。
- `ToolRunsResult` 保留自己的 ID overflow/ellipsis/nowrap；`ToolScheduleResult` 使用共享基础样式，不增加截断。
- 两个 renderer 继续保留各自 ToolRun 状态数据与呈现文案。

## 替代方案

- 合并后台与定时 ToolRun renderer：拒绝。它们的数据阶段和状态输出不同，重复的只是相同基础行排版。
- 让两边都截断 ID：拒绝。审计未证明定时操作 ID 需要当前不存在的截断行为。

## 影响与验证

仅合并基础 CSS 声明，保留原有 ID 溢出策略、行内容和状态语义。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（693 records）和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复两处本地 `.tool-run-row`/`.tool-run-id` 基础声明并撤销全局规则、命名规范和路线图更新。无数据迁移。
