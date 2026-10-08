# ADR 0751：HTTP renderer body props 对齐校验

## 状态

已采纳并实施。

## 背景

HTTP builtin renderer validator 对存在的 `body` 字段只接受字符串，同时允许字段缺省或为 `null`。`ToolHttpResult` 的 props 却把 `body` 声明为 `unknown`；组件本身只在它是非空字符串时渲染。

## 决定

- 将 `body` prop 收窄为可选 `string | null`，与 registry 的校验边界一致。
- 非字符串对象继续由 registry 回退到通用 JSON renderer，不传入专用组件。

## 影响与验证

只收紧 UI 内部 props，不改变 HTTP producer、ToolResult wire 或响应内容。新增非字符串 body 的负向 renderer 用例；通过 Svelte 类型检查和 UI 全量测试验证。

## 回滚

恢复开放 `unknown` 类型即可；没有 wire 或持久化迁移。
