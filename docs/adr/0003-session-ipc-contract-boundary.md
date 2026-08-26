# ADR 0003：会话 IPC 契约边界

日期：2026-08-26  
状态：已采纳

## 背景

会话生命周期事件原本有后端名称映射，但其载荷在 Agent 适配器和命令处理器中以临时
`serde_json::json!` 组装；前端监听器将所有 Tauri 事件视为 `any` 并在页面内直接读取
`snake_case` 字段。这样字段增删无法被编译器或单元测试发现，且命名转换分散在多个消费者。

## 决定

- 会话生命周期、错误、标题更新与删除事件使用 `events.rs` 中的命名 DTO 与事件常量；Agent
  适配器和会话命令共同复用它们。
- 前端以 `contracts/session.ts` 作为会话事件的唯一类型登记表，`events.ts` 的会话监听封装是
  唯一 snake_case → camelCase 转换点。路由和视图只能消费 camelCase 载荷。
- 保持现有事件名、字段含义、终态副发和 `session_id: null` 全量清空哨兵不变；不增加旧字段
  fallback 或事件别名。
- 用 `docs/ipc-contracts.md` 登记会话命令与事件的生产者、消费者、顺序、幂等和敏感字段限制。

## 替代方案

继续使用 `serde_json::Value` / `any`，或直接把 Agent 的 `SessionInfo` 序列化到 UI。前者会保留
隐式 wire shape；后者会泄漏输入、摘要与内部队列等不该跨端的数据。两者均被拒绝。

## 影响

前端会话监听器的内部字段从 `session_id` 改为 `sessionId`，但 Tauri wire payload 仍保持后端的
snake_case，不影响 Tauri 命令或已存储数据。其它事件域将在各自迁移时加入同一目录；本决定不
改变数据库 schema、快照或配置格式。

## 验证

`events.rs` 序列化测试固定 Rust wire shape；前端 `session.test.ts` 固定 camelCase 映射和全量
清空哨兵。完整验证执行 Rust workspace 测试、严格 Clippy、Svelte check、Vitest 与生产构建。

## 回滚与重置

可回滚为此前的临时 JSON 构造和页面字段读取，但不应并存两套字段。该变更不涉及用户数据，
无需重置数据库、配置或缓存。
