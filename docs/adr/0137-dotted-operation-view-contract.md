# ADR 0137：统一点号 operation view 与扩展工具入口

## 状态

已接受（2026-09-13）

## 背景

Builtin 工具曾同时以聚合根、带 operation 的窄 view、scope 字段和历史别名出现在模型目录与 UI 中。
例如文件能力有时显示为 `files`，有时显示为 `files.read_text`；system、haven、media 以及桌面能力
也存在同样的展示差异。这样会让模型、权限 key、结果卡片和设置页对“同一个能力”的正式名称产生不同理解。

## 决定

1. 所有 operation-based builtin 的模型入口统一使用 `root.operation` 点号名称。当前包括
   `files.*`、`system.*`、`process.*`、`clipboard.*`、`input.*`、`window.*`、`media.*`、
   `memory.*`、`agent.*`、`actions.*`、`schedule.*`、`preferences.*`、`checklist.*` 和 `haven.*`。
   例如文件读取正式名称为 `files.read`，系统环境变量读取为 `system.env.get`，媒体检查为
   `media.inspect`，Haven 诊断状态为 `haven.diagnostics.status`。
2. 聚合 struct/module（例如 `FilesTool`、`SystemTool`、`MediaTool` 和 admin capability tool）
   仍是唯一的执行、取消和原生 Tauri 边界，但不再以聚合根注册到模型目录。`OperationViewTool`
   负责固定 operation/scope，并发布只包含该能力参数的 provider schema。
3. 一个 operation view contract 同时声明名称、固定输入、schema、风险、幂等性、作用域、并发资源、
   权限 key、renderer、icon 和 prompt。后端 registry/security 与 UI contract 必须镜像同一正式名称。
   view 的权限 key 默认就是完整点号名称；父级 tool setting 仍可作为配置继承入口。
4. 已启用 Skill 直接注册为全局 `skill__...` 工具，删除过时的 `load_skill` loader 及其 session 恢复逻辑。
   MCP 维持不同的生命周期：保留 `load_mcp` 作为按需入口，并将已加载的 `mcp__...` adapter 注册到
   session catalog，因为远程 schema 和工具预算仍需要渐进式加载。
5. 运行时能力不足时仍可裁剪对应 view（例如 `window.ocr` 或媒体 provider 能力）；安全矩阵必须把
   这些可选路由标成可选，而不是借由展示名称重新引入聚合根。

## 影响与兼容性

- 新会话的模型 prompt、provider tool catalog、权限摘要、结果卡片和工具设置统一使用点号名称。
- 旧配置中的聚合根或 `load_skill` 权限不会迁移；配置加载器将其视为一次性备份/重置边界。
- 历史 transcript/step 可以由 UI 的 legacy parser 保留显示，但不得重新注册旧模型工具。
- 聚合实现名称可以继续存在于 Rust 代码中；它们是内部结构，不属于模型可见命名契约。
- 此变更不修改数据库 schema，也不改变 ID 格式。

## 验证

- `haven-tools` registry/security 回归测试覆盖每个已注册 view 及其风险、权限和可选能力。
- Rust 与 UI 的 operation-view contract 测试确认名称、schema、renderer 和 prompt 不漂移。
- Skill catalog、resume 和 MCP session catalog 测试确认 Skill 全局直注册、MCP 仍按需加载。
- 运行 workspace Rust 测试、严格 Clippy、UI 类型检查和 UI 单测。

## 回滚

若必须回滚，恢复本 ADR 对应提交并按 `docs/release-and-reset.md` 备份/重置旧工具权限与未完成快照；
不得把新旧聚合根和点号 view 同时重新暴露给模型。
