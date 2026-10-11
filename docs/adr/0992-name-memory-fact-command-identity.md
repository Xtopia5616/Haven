# ADR 0992：统一 Memory fact command DTO 的事实身份字段

## 状态

已采纳并实施。

## 背景

App 的 `MemoryFactResponse` 是 `list_facts` 与 `add_fact` 的 renderer wire DTO，显式 allowlist 能避免 repository model 的字段变更自动扩展 IPC。DTO 内的 fact 主键曾叫泛名 `id`，而其实体属于 `fact-*` 身份空间。

## 决定

1. `MemoryFactResponse` 将身份字段命名为 `fact_id`；`list_facts` 与 `add_fact` 一致使用该字段。
2. UI 复用 generated DTO，在 parser 中只接受 canonical `fact_id`，不接受旧泛名 `id`。
3. Repository `Fact.id` 与 SQLite 物理列 `facts.id` 保持其存储 owner 内部形状；App mapper 将其显式投影到 `fact_id`。
4. MemoryTool 的动态 recall 结果不包含事实主键；本决定不扩展工具输出，也不改 Agent 内部 contradiction prompt 的 payload。

## 替代方案

- 保留 `id` 并由 `Fact` 类型上下文解释：继续让 IPC 字段隐去实体身份。
- 同时输出 `id` 与 `fact_id`：会产生两份可依赖的身份字段，并保留已退役泛名。
- 将 repository 与数据库主键同步改名：超出 Tauri DTO 边界，不能提升存储/IPC owner 的清晰度。

## 影响与验证

- 生成的 `MemoryFactResponse` TypeScript shape、Memory command parser、长期事实视图与测试 fixture 改用 `fact_id`。
- Rust 序列化测试断言只输出 `fact_id`；UI 测试拒绝只提供 `id` 的响应。
- 无数据库 schema、持久事件、配置或 MemoryTool 输出变化。命令响应随 App/UI 一起发布。
- 验证包括 Rust workspace 测试与 Clippy、UI 类型检查/测试/build、IPC 契约和 ADR 索引检查、格式与 diff 检查。

## 回滚

回滚时 Rust DTO mapper、generated contract、UI parser 和消费者必须一并恢复为 `id`。无需数据库迁移或重置；回滚会重新引入无法从字段名直接辨认实体的 IPC 身份字段。
