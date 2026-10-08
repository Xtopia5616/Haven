# ADR 0764：主题 store 使用闭合 ThemeMode

## 状态

已采纳并实施。

## 背景

`themeStore` 只支持 `light` 与 `dark`，但 `detectInitialTheme`、store 内部状态、`applyTheme`、`setTheme` 和 `AppShell.theme` 都以开放 `string` 表达。有限 UI 状态因此可被错误传入 shell，且相邻函数签名没有体现相同值域。

主题初始化会读取 localStorage 和 `document.documentElement` 属性；这两个入口仍可能包含外部或旧值，必须在把数据写入 typed store 前验证。

## 决定

- 在 `themeStore.ts` 定义 `ThemeMode = 'light' | 'dark'`，并用于初始主题读取、当前状态、apply helpers、setter 与 `AppShell.theme`。
- 将 localStorage 与 document attribute 解码集中到 `isThemeMode`；无效初值回退到既有系统偏好/默认逻辑。
- `setTheme` 保留 runtime guard，错误的动态调用不改变状态；正常生产调用在编译期只接受 `ThemeMode`。

## 影响与验证

只收窄 UI 内部状态与 props；DOM 属性、localStorage key/value、偏好探测和切换行为不变。固定 Node 24.20.0 下的 UI check 与完整 test suite 通过。

## 回滚

恢复 `string` 签名并删除 `ThemeMode` 即可；无 IPC、配置或数据库迁移。
