# ADR 0712：共用 Tool result 窗口与显示器行基础样式

## 状态

已采纳并实施。

## 背景

`ToolWindowResult` 与 `ToolSystemResult` 的窗口/显示器结果行复制了相同的 flex 排版、间距、圆角、字号、主文本截断和隔行底色。两个 renderer 还复用了含义不符的 `.window-pid` 和 `.window-title`：System 把显示器分辨率称为 PID、把显示器名称称为窗口标题；Window 则把 `condition` 与 `control_type` 称为 PID。

## 决定

- 全局 `.tool-result-window-row` 拥有行排版和隔行底色；`.tool-result-window-primary` 拥有主内容的等宽字体、截断与颜色；`.tool-result-secondary-value` 拥有次级值样式。
- Window 保留 `window-title` 表达标题；PID、condition 和 control type 的次级展示统一标为 `window-meta`。
- System 的显示器名称和分辨率分别标为 `display-name` 与 `display-resolution`。
- 两个 renderer 继续拥有各自的数据 shape、字段解释与内容。

## 替代方案

- 合并两个 renderer：拒绝。它们分别呈现操作系统窗口与显示器数据，字段和数据来源不同。
- 继续使用 `.window-pid` / `.window-title` 表示显示器与非 PID 内容：拒绝。类名与实际概念冲突，误导后续样式维护。
- 保留重复的局部基础样式：拒绝。被复用的是同一行与文本排版规则，已有明确全局样式 owner。

## 影响与验证

只调整 UI 内部 class、语义命名与 CSS owner；数据、行内容和视觉属性不变。无 IPC、数据库或配置变化，无需重置。验证通过：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR index 与 `git diff --check`。

## 回滚

恢复两个组件的局部窗口/显示器基础样式，并还原原 class；撤销命名规范、路线图和本 ADR。无数据迁移。
