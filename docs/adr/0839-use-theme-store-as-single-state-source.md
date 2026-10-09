# ADR 0839：以 themeStore writable 作为唯一状态源

## 状态

Accepted — 2026-10-09

## 背景

`themeStore.ts` 同时保存可变模块变量 `currentTheme` / `currentAccent` 和一个 `{ theme, accent }` Svelte writable。每个 setter 都要双写这两份状态；公开 getter 读模块变量，Svelte subscriber 读 writable snapshot。两种读取路径目前由同一 setter 保持同步，但新增更新路径若漏写其中一份，就会让 Settings、AppShell 和订阅者看到不同主题。

## 决定

- 以 `themeStore.ts` 内部 writable snapshot 作为 theme/accent 的唯一可变状态源。
- `currentTheme`、`currentAccent`、`accentColor` 与 `isPreset` getter 从该 snapshot 派生，不另存字段。
- DOM attribute、CSS token 与 localStorage 仍是由 store setter 更新的外部副作用；它们不是可写 UI state owner。
- 保持主题/强调色校验、首次读取优先级、属性更新顺序、storage key、色彩对比计算及公开 store API 不变。

## 替代方案

- 保留模块变量与 writable 双写：拒绝。两个本地状态对象表达同一事实并要求每个 setter 同步维护。
- 将 DOM 或 localStorage 作为 getter 的实时状态源：拒绝。二者是不可信/外部入口；运行时状态仍应由 store 持有，storage 和 document 只在初始化时解析并校验。

## 影响与验证

仅合并 UI 进程内主题状态源，不改变 localStorage 格式、DOM、CSS tokens、Tauri 配置或用户数据，无需重置。验证：主题 store 全量测试、Svelte 类型检查、UI 全量测试与生产构建、Prettier 和差异检查。

## 回滚

恢复模块级 theme/accent 变量及 setter 双写，恢复旧 getter 读取点；无需数据或配置重置。
