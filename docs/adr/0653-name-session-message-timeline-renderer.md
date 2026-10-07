# ADR 0653：按 Session 职责命名消息时间线 renderer

## 背景

`SessionTimeline` 是当前 Session 页面入口，负责加载中、空状态和结束提示；它把已有消息交给 `ChatMessageTimeline`。后者实际只用于 Session 页面，接收 `SessionMessage`、session ToolRun 和 SessionRun 结束状态并渲染消息、活动与工具运行。它没有通用 Chat 消费者，旧 `Chat` 前缀是 ADR 0048 拆分时间线时留下的名称。

前一轮 ADR 0559 已统一消息 shape 和时间线领域类型，但遗漏了该 renderer 的组件文件名及其外层类型引用，导致项目术语规则与实现仍不一致。

## 决定

- 将 `ChatMessageTimeline.svelte` 改名为 `SessionMessageTimeline.svelte`，调用方、props 类型和注释同步使用新名。
- 保留 `SessionTimeline` 与 `SessionMessageTimeline` 两个组件：外层拥有加载/空状态语义，内层负责已有消息、活动和 ToolRun 的呈现。当前是静态组件引用，不声称它提供动态 import 或 bundle 延迟加载；保留边界的依据是两者负责不同的 presentation 状态。
- 仅修改 UI 内部模块名；DOM、props、消息顺序、组件挂载条件与交互行为不变。无兼容别名。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`

## 回滚与重置

代码回滚时将文件名和引用恢复为 `ChatMessageTimeline`。本次不改变 IPC、配置、数据库或用户数据，不需要重置。
