# ADR 0527：从 Rust enum 生成 Interaction 事件契约

## 状态

已采纳（2026-10-06）。

## 背景

`haven_agent` 已定义 `InteractionKind` 与 `InteractionStatus`，但 Tauri 的
`InteractionRequestedEvent` 把字段写成 `String`。前端因此还要维护一份 TypeScript
联合类型和两处 runtime 允许值列表；resume 和 live event 的 wire 值可能随时间漂移。

Interaction owner 和生命周期实现则有意分属 Session、ScheduledToolRun、AppCommand 等路径。
本切片只统一字段词汇来源，不改变路由、持久化或运行时 owner。

## 决定

1. `InteractionRequestedEvent.kind/status` 使用 Agent 的 `InteractionKind` / `InteractionStatus`。
   `project_interaction` 直接投影这些类型；Serde 的 snake_case JSON 值保持不变。
2. IPC 类型生成器为简单的 Rust 字符串 enum 导出 runtime value tuple，并从 tuple 定义
   TypeScript union。App event mapper 与 resume normalizer 使用生成的 Interaction tuple 校验输入。
3. `InteractionOwner` 继续作为独立的显式路由元数据。Ask、确认与定时确认沿用各自 owner、
   continuation 和持久化流程。

## 影响

- Rust enum 是 Interaction kind/status 类型和值集合的单一契约来源。
- `interaction:requested` 与 resume 的 JSON shape、字段名和值不变；不涉及数据库 schema、持久化、
  配置或缓存，无需重置数据。
- Generator 为现有简单 string enum 同时导出 TS runtime tuple，现有 union 类型可由该 tuple 派生。

## 验证

- IPC contract 生成与 drift 检查。
- Rust workspace 格式、编译、Clippy 与测试；UI 检查、测试和生产构建。

## 回滚

恢复 DTO 的 String 字段与旧 mapper，并从生成器撤回 string enum runtime tuple 输出；wire 值和持久数据无需迁移。
