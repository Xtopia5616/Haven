# ADR 0347：Agent event contract validation boundary

> ADR 0380 supersedes this decision's acceptance of unknown tool outcome, retry, idempotency, and operation-scope values. Unknown additive fields and explicitly dynamic payloads remain ignored/preserved as described here.

- 状态：已采纳（2026-09-25）
- 范围：`events.rs` 的 Agent wire DTO、`event_bridge.rs::TauriEmitter`、`contracts/agent.ts` 与 `events.ts` listener adapter
- 关联：[ADR 0330](0330-session-lifecycle-ui-contract-mapper.md)、[ADR 0335](0335-action-board-ui-contract-mapper.md)、[ADR 0346](0346-app-event-listener-contract-boundary.md)

## 背景与审计

Rust `events.rs` 的具名 DTO 是 Agent Tauri payload 的 wire 权威，`event_bridge.rs::TauriEmitter` 负责变体到 channel/payload 的适配。`contracts/agent.ts` 同时声明 camelCase 消费 DTO 和重复的 snake_case wire interfaces；已有 `mapAgentEvent` 是唯一字段映射点，但输入依赖这些 wire interfaces 的类型断言，没有检查原生事件的 envelope 或必需字段。

listener 审计没有发现绕过 mapper 的 Agent 消费者：聊天页和布局都经 `agentEventListeners`。聊天页消费 transcript/stream、tool preview、usage、compaction 与 media plan；布局只消费 `agent:stream_stalled` 的模型状态和 `notification:show` 的全局 toast，Agent channel 集合互不重叠。它们共用同一个 `appSessionReducer`。media plan 的存储与说明 toast、系统通知与应用内 toast 各自有独立副作用 owner。另发现既有 session terminal fan-out：`SessionCompleted`/`SessionError` 各发送一个 `session:completed`/`session:error` 主事件和一个 `session:updated` secondary event；聊天页两个 handler 都做终态 reducer/cleanup，其中 active session 的 live-message finalization 会重复调用。跨 channel 没有共享 event identity，当前切片不改 session contract/reducer，所以保留并记录为后续审计风险。`interaction:requested` 属于 app event；resume response 的 interaction normalizer 仍有独立的恢复兼容职责，本 ADR 不触及。

## 决定

1. 保持 Rust DTO、Tauri channel、payload、producer 和 listener 到达顺序不变。删除 `contracts/agent.ts` 中重复的 snake_case wire interfaces；camelCase DTO 是消费侧类型，Rust DTO 仍是 wire 事实来源。
2. `mapAgentEvent` 接受 `unknown`，验证 Tauri event 名、有限数值 event id、对象 payload、必需字段和嵌套数组/envelope 的类型，再执行唯一 snake_case → camelCase 映射。无效 envelope 或必需字段返回 `null`；`agentEventListeners` 丢弃该事件并记录不含 payload 内容的通用 warning。
3. 映射只输出已知 UI 字段，忽略未知附加字段；动态 tool input、WebSearch result 和 usage cache diagnostics 保留为原值。Outcome、idempotency、operation scope、media/RequestKind、inject source 和 usage call kind 等 enum-like 字符串不做封闭校验，以接受未来字符串值。未知 usage `call_kind` 仍到达既有 handler，由其原有 error log + skip fallback 处理。
4. 保留空 notification title/body，由布局现有默认标题/正文逻辑处理；Rust error sanitization、`session:error` reducer 路径、session/interaction/usage ownership、通知顺序与系统通知行为不变。已审计到的 SessionCompleted/SessionError 双 channel fan-out 和终态 cleanup 重叠不在本切片修改。
5. 不修改 session/action/recording/settings contracts、resume interaction normalizer、Rust wire DTO、全局 codegen 或事件 producer。

## 替代方案

- 保留两套手写 TS wire/consumer interface：继续重复 Rust 字段并使运行时 payload 形状未验证，拒绝。
- 把字段转换移入各 route/store：会形成多个 snake_case 读取入口，拒绝。
- 封闭 Agent enum 或合并 toast/store/listener 副作用：会改变未知字符串降级及既有副作用 owner，拒绝。
- 引入跨域 Rust→TS codegen：超出当前 agent event 窄切片范围，留待稳定的多域生成策略评估。

## 影响与验证

Agent wire channels、字段、事件顺序、通知、session reducer 行为及动态扩展 payload 不变。新增回归覆盖未知 enum 字符串与附加字段、malformed payload 拒绝、listener 顺序/不含 payload 的 warning，以及空通知文案保留。剩余风险是上述双 channel terminal fan-out 让聊天页重复执行部分幂等 cleanup；本切片不改变既有 session 行为。无 Rust、持久化、配置或数据重置变更。

验收命令：

```sh
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
corepack pnpm --dir ui run build
pwsh -NoProfile -File scripts/check-ipc-events.ps1
pwsh -NoProfile -File scripts/check-ipc-contracts.ps1
git diff --check
```

## 回滚

回滚本切片可恢复旧 wire interface 与 unchecked mapper；无需 Rust 修改、IPC 变更、配置迁移或数据重置。
