# ADR 0705：共用 Tool result 滚动样式

## 状态

已采纳并实施。

## 背景

通用 Tool result list、Admin renderer 与 Memory renderer 分别声明了相同的 `overflow-y: auto` 和 extra-small 圆角。它们的限高不同：通用结果 200px、Admin 220px、Memory 240px。重复声明让滚动行为和圆角由多个 owner 维护，而各自高度属于真实的页面密度选择。

## 决定

- `.tool-result-scroll-area` 在 `ui/src/app.css` 唯一拥有 max-height、纵向滚动和圆角。
- 默认高度保持 200px；Admin 与 Memory 通过局部 CSS 自定义属性覆盖为 220px 与 240px。
- 保留 `admin-list` 和 `memory-list` 作为各自 row layout 的领域样式，只由它们指定滚动区域高度。

## 替代方案

- 把三个高度统一为一个值：拒绝。差异目前是各 renderer 的实际布局选择，审计没有证据支持改动用户可见尺寸。
- 保留三份滚动声明：拒绝。overflow 与圆角行为相同，重复维护没有独立 owner 价值。

## 影响与验证

仅合并 CSS scroll/shape 声明，保留各处现有滚动高度、列表内容和分页行为。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`（0 errors、0 warnings）、`corepack pnpm run test:run`（124 files、986 tests passed）、ADR index（688 records）和 `git diff --check`。Vitest 输出 `TimeoutNaNWarning`，退出码为 0。

## 回滚

恢复 Admin、Memory 的本地滚动声明，并撤销全局变量与命名规范/路线图更新。无数据迁移。
