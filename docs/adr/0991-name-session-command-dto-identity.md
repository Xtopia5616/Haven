# ADR 0991：统一 Session command DTO 的会话身份字段

## 状态

已采纳并实施。

## 背景

历史列表、会话 lineage 和 resume 命令都返回 `SessionRecordDto`。该 DTO 表示持久会话实体，但用泛名 `id`；同一响应中的关联字段已经叫 `session_id`。UI parser 因此必须依赖 DTO 上下文来推断 `id` 的实体类型。

## 决定

1. `SessionRecordDto` 的 Rust 与 Tauri wire 字段统一命名为 `session_id`，包括历史列表、lineage parent/child 与 resume 响应中的嵌套记录。
2. UI 校验器、renderer 消费者和测试 fixture 直接使用 `session_id`，不为 Haven 自有命令响应保留 `id` alias。
3. 数据库物理主键 `sessions.id` 保持不变，Repository 与 command adapter 在映射时将其投影为 `session_id`。
4. 本决定不包含 Agent runtime `SessionInfo`/`StepInfo` 和 Memory `SessionStep`；它们是后续实体字段审计的独立边界。

## 替代方案

- 保留 `id`，让 UI 继续按 DTO 类型上下文解释身份：没有消除字段歧义。
- 同时输出 `id` 与 `session_id`：会形成两个可被消费者依赖的字段，并违反 Haven 自有 wire contract 不保留旧名 alias 的规则。

## 影响与验证

- 变更只影响随应用一起发布的 Tauri command response；没有数据库列、持久事件或用户配置迁移。
- IPC 生成器更新 `generatedCommands.ts`；历史、lineage 和 resume 的 runtime parser 校验 `session_id`，并拒绝只携带旧 `id` 的结果。
- 完成后运行 Rust workspace 测试与 Clippy、UI 类型检查和测试、UI build、IPC 契约检查、ADR 索引检查及格式/差异检查。

## 回滚

若必须回滚，Rust DTO 和 UI parser/consumer 应作为一个版本原子恢复为 `id`。无数据库回滚或重置步骤；回滚会重新引入依赖上下文解释的泛名身份字段。
