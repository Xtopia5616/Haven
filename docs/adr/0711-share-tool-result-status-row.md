# ADR 0711：共用 Tool result 状态摘要行

## 状态

已采纳并实施。

## 背景

Agent 与 HTTP renderer 分别声明了 `.action-row`，两者都由状态 badge 和关联值构成，CSS 的 flex、center alignment、gap、字体大小和行高逐项相同。`action-row` 是泛名，也没有体现它承载的是 Tool result 状态摘要。

## 决定

- 两处统一使用 `.tool-result-status-row`。
- 共用行布局由 `ui/src/app.css` 唯一拥有。
- Agent 会话/子任务状态与 HTTP response status 的字段和 badge 继续由各自 renderer 拥有。

## 替代方案

- 保留两个本地 `.action-row`：拒绝。样式与呈现角色都相同，继续拆分只造成重复和泛名。
- 合并 Agent 与 HTTP result component：拒绝。状态字段、状态映射和附加行内容完全不同，只有行布局相同。

## 影响与验证

仅统一内部 class 和样式 owner；对齐、间距、字号和行内容不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（694 records）和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复 Agent/HTTP 各自的 `.action-row` CSS 和 markup class，撤销全局样式、命名规范与路线图变更。无数据迁移。
