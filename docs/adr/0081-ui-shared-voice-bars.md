# ADR 0081：UI 共享声波条动画原语

## 背景

`LoadingState` 和 `RecordingIndicator` 都渲染声波条动画，但各自维护 bar DOM、动画关键帧、节奏、颜色和 reduced-motion 规则。两者的业务语义不同，不能直接合并成同一个加载或录音组件；重复的视觉实现则容易发生漂移。

## 决定

- 新增无业务状态的 `VoiceBars` 视觉原语，统一声波条数量、动画模式、语义颜色和状态数据属性。
- `LoadingState` 使用 `float` 模式；`RecordingIndicator` 使用 `equalizer` 模式，并继续由自身决定录音静默、说话和转写状态。
- `VoiceBars` 不读取 store、不调用 Tauri、不处理录音或加载生命周期，只负责可访问性隐藏的装饰性动画。
- 动画降级统一在 `VoiceBars` 内处理，`prefers-reduced-motion` 时停止动画。

## 替代方案

- 直接让 `LoadingState` 复用 `RecordingIndicator`：会把录音计时、VAD 和取消语义带进加载状态，边界错误。
- 只抽共享 CSS：仍会保留两套 bar DOM 和 class 约定，后续变更容易再次分叉。

## 影响与回滚

本次只改变前端视觉组件组合，不改变 IPC、录音事件、加载生命周期或用户数据，不需要重置。回滚本提交即可恢复两个组件各自的声波条实现。

## 验证

- `cd ui; corepack pnpm run check`
- `cd ui; corepack pnpm run test:run`
- `cd ui; corepack pnpm run build`
