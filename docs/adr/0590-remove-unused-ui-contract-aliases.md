# ADR 0590：删除未使用的 UI contract alias

## 状态

已采纳并实施。

## 背景

UI contract modules 中有若干 generated DTO 的导出 alias 没有生产或测试消费者，包括 memory 的 `FactSourceRef`、tools 的 `McpTransport`，以及 session-history 的信息步骤、旧 resume interaction 和 resume message/step input 转发名。它们扩大了可见命名表，却没有作为领域 façade 被调用。`MediaSettings` 还把 generated 的 `MediaInputStrategyInput` 局部改名为 `MediaInputStrategy`，与 `ModelSettings` 的直接引用不一致，并弱化了“请求输入类型”的角色。

## 决定

1. 删除本轮扫描中只有声明/导入、没有消费者的 contract alias，并清除因此无用的 generated type-only imports。
2. `MediaSettings` 直接引用 generated `MediaInputStrategyInput`；配置值域与运行时行为不变。
3. 保留被消费者使用的领域 contract façade，例如 `Fact`、`MemoryRecallItem`、MCP server view DTO 与 Session resume response。

## 替代方案

- 保留所有生成 DTO 的方便转发名：拒绝，未消费的 export alias 让同一 wire 概念拥有不必要的第二个名字。
- 将 renderer 输入枚举改为闭合运行枚举：拒绝，`Input` 代表设置写入边界的可接受输入值域，应保持与 generated contract 一致。

## 影响与验证

- 不改变 Rust DTO、生成文件、IPC 名称、JSON shape、配置值或 UI 行为；仅收窄未使用的 TS 导出。
- 更新命名规范与架构路线图；无需持久数据重置。
- 验证通过：`corepack pnpm run check`、`corepack pnpm run test:run`（122 files / 975 tests）、`corepack pnpm run build`、ADR 索引与 `git diff --check`。

## 回滚

恢复被删除的 type-only 导入/alias 和局部类型名；不涉及生成器或持久数据。
