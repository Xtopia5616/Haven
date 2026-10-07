# ADR 0668：删除未使用的 Tool operation helper 并收紧列表 snippet 类型

## 背景

UI 全仓引用扫描发现 `toolIdentity.ts::toolOperationName` 只有定义，没有调用方。内置工具设置页已直接从 `ToolManifestView.identity.operation` 投影 operation 名称，这个 helper 没有独立规则或转换。

`ToolResultList<T>` 的 children contract 已声明为 `Snippet<[T[]]>`。21 个调用点又在 snippet 参数上写 `any[]` 默认值，重复弱化组件提供的列表类型。Settings 页的一处 MCP server mapper 也对 generated settings 字段添加了无必要的 `any` 注解；voice submit wrapper 把 `ProcessResult` 写成 `Promise<any>`。

## 决定

- 删除没有消费者的 `toolOperationName` helper 和它唯一用途的 manifest import。
- `ToolResultList` 调用点直接接收 snippet 参数，不再添加 `any[]` 默认值或显式 `Record<string, any>[]`。
- Settings server mapper 直接使用 generated settings 类型；`submitVoiceTranscript` 明确返回 generated `ProcessResult`。
- 保留 `SESSION_LIFECYCLE_KINDS`。它通过 `satisfies Record<GeneratedSessionLifecycleEvent['type'], true>` 在编译期要求 renderer mapper 清单覆盖生成的所有 lifecycle variant，虽然不需要 runtime 读取。
- 本切片不改变 Tool JSON runtime shape、IPC、配置、数据库或持久数据；动态 Tool result 字段的 validator/type owner 继续作为 UI 审计项。

## 验证

- `corepack pnpm run check`
- `scripts/check-adr-index.ps1`

## 回滚与重置

恢复 helper 和局部类型注解即可回滚；没有运行时行为或用户数据变化，无需重置。
