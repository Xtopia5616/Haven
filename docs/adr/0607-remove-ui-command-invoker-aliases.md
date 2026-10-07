# ADR 0607：删除 UI command invoker 同义 alias

## 状态

已采纳并实施。

## 背景

`ChatInvoke` 与 `SessionHistoryInvoker` 都只是 generated `TauriCommandInvoke` 的完整类型别名。前者用于 Chat session controller dependency，后者用于 `getSessionForResume` 的可注入参数；两者没有缩窄可调用的命令集合，也没有增加请求或响应约束，因此同一注入契约出现了多个局部名称。

## 决定

1. 删除两个 feature-local alias。
2. controller dependency 与 session-history wrapper 参数直接引用 `TauriCommandInvoke`。
3. 未来只有在 invoker port 缩窄命令能力或增加明确的输入/输出约束时，才定义专用接口。

## 替代方案

- 保留 alias 并仅统一名称：拒绝，仍然重复表达相同的完整 invoke 契约。
- 为两个消费者分别定义更窄的接口：拒绝，现有消费者和测试注入没有这类能力边界或额外约束。

## 影响与验证

- 仅改变 UI 内部 TypeScript 类型名称；运行时调用、测试注入方式、Tauri command 集合及 wire shape 不变。
- 命名路线图 §5.7 继续保持 Active；UI 组件、stores、controllers、props、事件 handler 和跨层契约仍待全量审计。
- 验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引及 staged diff 检查。

## 回滚

恢复两个 alias 并将对应字段和参数改回局部类型名；无持久化或 wire 迁移。
