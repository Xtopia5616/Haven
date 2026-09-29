# ADR 0408：UI 统一动画方案

- 状态：已采纳
- 日期：2026-09-29

## 背景

UI 动画根据系统的 `prefers-reduced-motion` 设置切换为另一套表现：有的缩短或取消过渡，有的停止循环动画。弹窗还通过运行时 `matchMedia` 选择不同的时长和缩放参数，导致动画规则分散在 CSS、组件逻辑和文档中。

## 决定

- UI 动画统一使用各组件定义的时长和效果，不再读取 `prefers-reduced-motion`，也不提供 reduced/normal 两套动画分支。
- 删除弹窗动画 profile helper、组件中的 `matchMedia` 分支，以及 CSS/HTML 中 `prefers-reduced-motion` 覆盖规则。
- 相关动效设计见 [UI 规范 §3.4](../ui.md)。

## 替代方案

- 保留系统偏好并将降级集中到全局样式：仍会形成正常和 reduced 两套动画表现，不符合本次统一方案。
- 为弹窗保留单独的运行时 profile：与其他组件的 CSS 降级并存，会继续分散动画策略。

## 影响

即使 Windows 启用了减少动态效果，Haven UI 仍会播放组件定义的动画。仅改变前端动效策略，不影响数据库、配置、IPC 或用户数据；此前有关 reduced-motion 的决定只在该规则上被本 ADR 替代。

## 验证

- 全仓搜索确认运行时代码不再包含 `prefers-reduced-motion` 或动画用的 `matchMedia` 分支。
- `git diff --check`。

## 回滚与重置

恢复相关 CSS/HTML media query、组件时长选择逻辑和弹窗 profile helper 即可；无需重置数据。
