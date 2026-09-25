# ADR 0330：Session lifecycle UI contract mapper

- 状态：已采纳（2026-09-25）
- 范围：`crates/app-binary/src/events.rs`、`ui/src/lib/contracts/session.ts`、`ui/src/lib/events.ts`

## 背景

Rust session lifecycle DTO 是 Tauri wire payload 的权威来源。此前前端在
`contracts/session.ts` 同时维护了与 Rust 字段重复的 wire interfaces 和 camelCase UI DTO，
mapper 又依赖编译期类型断言；真实 Tauri payload 的字段缺失或类型错误可能透传为
`undefined`。此 payload 被聊天页、根布局和 MemoryView 多处监听，适合作为 Phase 8 首条正式
收口的 UI contract mapper 切片：复用已有 `mapSessionEvent`，移除 wire mirror 并补运行时校验。

## 决定

- 保留 Rust DTO 和 channel 原样：`SessionLifecycleEvent`、`SessionErrorEvent`、
  `SessionTitleUpdatedEvent`、`SessionDeletedEvent` 定义在 `crates/app-binary/src/events.rs`。
- 在 `ui/src/lib/contracts/session.ts` 保留一个 `mapSessionEvent` 作为 lifecycle event 的
  唯一 snake_case → camelCase 映射点；移除与 Rust wire 字段重复的 TS wire interfaces，读取
  Tauri payload 时从 `unknown` 做运行时字段验证。
- 必需字段缺失或类型错误、未知 event 名称或无效 event envelope 时返回 `null`；
  `events.ts` 的 `sessionEventListeners` 与 `registerSessionListener` 共用一个适配 helper，
  丢弃该事件并记录不含 payload 内容的 warning。handler/controller/reducer 只接收完整的
  camelCase DTO。
- 忽略未知附加字段以容纳 wire 侧的兼容扩展；可选 `waiting_reason` 与 `reason` 缺省时仍规范化为
  `null`。未知 status 继续降级为 `error`，未知 waiting reason 继续降级为 `null`。
- 不做 codegen，也不引入新的事件登记点。这个 mapper 是 Phase 8 第一条显式验证的 UI contract
  边界；其余 action、agent、app、recording 事件与 command request/response 镜像仍待后续生成收口。

## 替代方案

- 继续为 wire payload 保留一套手写 TS interfaces：会继续重复 Rust DTO 字段，拒绝。
- 让每个 handler 分别读 snake_case 并转换：会扩散映射逻辑并制造多个边界，拒绝。
- 立刻引入全域 codegen：本切片涉及多个事件域与命令，超出窄切片范围；待 mapper/contract 边界稳定后另行评估。

## 影响与验证

- Rust/Tauri event channel、payload 字段、生产者、消费者、顺序、幂等要求、session reducer 转换和
  Svelte 5 响应式行为均不变。
- Mapper 测试覆盖已知 payload、未知附加字段/事件、可选字段、未知 enum 降级及 malformed 必需字段；
  listener 测试确认 malformed event 不进入 handler。
- 验收：`cd ui; corepack pnpm run check`、`corepack pnpm run test:run`、
  `corepack pnpm run build`、`cargo check --workspace --locked`。
- 其余手写 contract mirror 范围与扩展顺序记录在
  [架构降复杂度重构路线图](../architecture-refactor-roadmap.md#阶段-8ipc-单源生成与-ui-编排收口)。

## 回滚

可直接回滚 UI mapper、测试、ADR、架构和路线图变更；没有数据库、配置或持久化变化，无需数据重置。
