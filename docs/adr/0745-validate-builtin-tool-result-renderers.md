# ADR 0745：在 UI builtin renderer 边界校验 ToolResult 输出

## 状态

已采纳并实施。

## 背景

`ToolResult.output` 是异构工具输出的 `serde_json::Value`。builtin producer 混用类型化 operation 与逐操作 JSON，MCP/Skill 还允许扩展 payload；它们没有共同的输出 DTO 或 JSON Schema owner。UI 根据 manifest renderer 选择组件，并对 `files.media`、`files.results` 和 `system.scope` 做二次分派。只检查顶层 record 会让 `null` 数组项或错误类型的嵌套对象进入依赖具体字段的模板。

## 决定

- 保留 `ToolResult.output` 的开放动态 JSON 边界，不新增全局 DTO 或 output schema。
- 在 UI renderer registry 调用专用组件前，按最终 renderer 校验该组件实际消费的可选字段、嵌套对象和数组项；校验只检查形状，不转换或删除 payload 字段。
- 已知 builtin payload 不符合 renderer 形状时回退 `ToolJsonResult`，仍展示原始 JSON。`files.media`、文件搜索结果与 `system.scope` 分派选定目标后才执行对应校验。
- 未知 renderer（包括 MCP/Skill）继续使用通用 JSON renderer，其扩展输出不受 builtin 规则约束。
- 为 Admin、File、Process、System 结果组件的 `data` props 和 File 的 `rawText` 标注显式 TypeScript 类型；这些静态类型不替代运行时校验。

## 替代方案

- 给所有 ToolResult 强加单一输出 DTO/Schema：拒绝。不同 builtin operation 与 MCP/Skill 的输出形状异构，统一 schema 会错误地收窄扩展边界。
- 遇到畸形结果时丢弃卡片或显示错误：拒绝。原始 JSON 仍可查看，避免校验失败隐藏工具的实际结果。
- 只在每个 Svelte 模板里添加局部数组过滤：拒绝。组件选择和 family 分支应在 registry 边界先完成统一校验，避免某个专用模板遗漏防护。

## 影响与验证

工具执行、Rust 序列化、provider wire、MCP/Skill payload 和数据库均不变。错误形状的 builtin 结果仍可见，但以 JSON 展示；未知额外字段继续保留。renderer props 与相关 type alias 引用审查未发现 declaration-only alias：`MemoryFact`、`MemoryHit`、`ToolRunSummary` 是 renderer props 的行形状，`ParsedToolResult` 是 parser 返回 union；validator/registry helper aliases 均有生产引用。

UI 验证：`corepack pnpm --dir ui run check`；`corepack pnpm --dir ui run test:run`（125 个测试文件、996 个测试）。

## 回滚

回滚时移除 renderer guard 并恢复旧的 registry 直接分派；恢复四个组件的隐式 props typing 只影响编译期检查。没有持久化或 wire 迁移，也不需要重置数据。
