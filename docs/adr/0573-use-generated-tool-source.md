# ADR 0573：工具来源分类复用生成枚举

## 状态

已采纳并实施。

## 背景

Rust 的 `haven_common::tools::ToolSource` 由 generated IPC contract 导出，值固定为 `builtin`、`skill`、`mcp`。UI `toolIdentity.ts` 又手写同一 closed union 作为卡片分类函数的返回类型，但该 alias 没有其它消费者，也没有独立展示值。相邻 `toolManifest.ts` 的 `ToolManifestSource` 则有意允许未来未知字符串，以便 parser 保留开放 manifest 值。

## 决定

1. `toolIdentity.ts` 直接导入 generated `ToolSource`，删除本地同名 union。
2. 保留 `toolManifest.ts::ToolManifestSource`，因为它承载的 parser 约束是开放字符串，与 generated closed enum 不同。
3. 卡片来源判定与标签行为不变；`ToolPresentation.represented_source` 仍是卡片显示来源，`ToolIdentity.source` 仍描述工具实现来源。

## 替代方案

- 把开放 parser 来源收窄为 generated `ToolSource`：拒绝，会丢弃未来来源值并改变 parser 契约。
- 为卡片分类再造 `ToolCardSource`：拒绝，值集合与生成枚举完全相同，没有独立 runtime state 或消费者形状。
- 将显示来源和实现来源强行合并为同一字段：拒绝，`load_mcp` 等 activation tool 的实现来源与代表来源可以不同。

## 影响与验证

- 只删除一个 UI utility 的重复 TypeScript union；parser、工具卡片 badge 和 wire shape 不变。
- 无 Rust、Tauri、数据库、配置或持久数据变化。
- 验证：UI `check`、`test:run`、`build`、ADR 索引与差异空白检查。

## 回滚

在 `toolIdentity.ts` 恢复本地三值 `ToolSource` union，并撤回对应命名规则与审计记录。
