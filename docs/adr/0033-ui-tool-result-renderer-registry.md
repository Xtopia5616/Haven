# ADR 0033：UI 工具结果渲染注册表

## 背景

`ToolResultCard.svelte` 同时承担公共折叠卡片、上下文菜单、参数展示、流式输出和
十余种工具结果模板。简单的 shell、通知、generic JSON 与 raw 文本分支不需要
访问复杂工具状态，却被迫和单一组件一起编译、回归。

## 决定

- 新增 `ui/src/lib/toolResultRenderers.ts`，按规范化结果 kind 注册 body renderer。
- 新增 `ToolShellResult.svelte`、`ToolNotifyResult.svelte` 和 `ToolJsonResult.svelte`，
  分别承载 shell、通知以及 generic/raw body；`ToolResultCard` 只负责公共卡片壳、
  交互状态和复杂工具分支。
- 保持 `parseToolResult`、工具标签、折叠行为、live/background 输出、复制菜单和
  现有 CSS 结果不变；复杂工具的 data-specific renderer 后续按同一注册表继续收口。

## 替代方案

- 继续在 `ToolResultCard.svelte` 添加分支：会使卡片组件持续膨胀，拒绝。
- 以 `{@html}` 渲染字符串模板：会绕过 Svelte 的转义和组件边界，拒绝。
- 本片一次性重写所有工具 renderer：回归面过大，暂不采用。

## 影响

这是 UI 内部渲染拆分。工具事件、结果 JSON、IPC DTO、持久化和视觉文案保持不变；
新增 renderer 组件共享 Material CSS 变量，不需要数据或配置迁移。

## 验证

```text
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

既有 ToolResultCard 测试覆盖 shell、notify、generic、raw、live/background 和折叠
行为；注册表返回的组件由这些渲染回归间接覆盖。

## 回滚与重置

代码回滚时删除三个 renderer 组件与 `toolResultRenderers.ts`，恢复 `ToolResultCard`
的四个内联 body 分支；本次不改变持久化数据或配置，不需要用户重置。
