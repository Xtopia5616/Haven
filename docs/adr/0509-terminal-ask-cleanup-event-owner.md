# ADR 0509：终态 Ask 清理由首个事件通道拥有

## 状态

已采纳；实现与 UI 门禁通过（2026-10-05）。

## 背景

Agent 将一次 completed/error 生命周期作为主 `session:completed` / `session:error` 事件和副 `session:updated` 事件发送，两者共用短期 `occurrence_id`。事件桥接对两个发送分别记录失败，因此 UI 可能只收到副 `session:updated`。前端已允许这个副通道独立认领终态消息清理，但 Ask 交互清理只存在于主事件 handler；副事件单独到达时会保留活跃会话的 pending Ask。

`claimTerminalCleanup` 已按 occurrence ID 实现首个通道 first-wins，并有两种到达顺序的消息清理测试；Ask 清理没有遵循同一个 owner 规则。

## 决定

1. 首个成功调用 `claimTerminalCleanup` 的终态事件通道同时拥有活跃会话 Ask 清理。独立 `session:updated` completed/error 若先到，清 Ask 并完成既有 UI 清理；其配对主事件不重复清 Ask。主事件先到时沿用同样的 first-wins 规则。
2. 只清理当前 active session 对应的 Ask；inactive session 更新不改变当前会话交互。paused 等其他非终态 status 不清 Ask；现有 pending/resume 清理继续保留。
3. 无 occurrence ID 的独立 `session:updated` 继续按现有规则认领自身清理。主/副事件 wire shape、occurrence identity、backend 生产者和 IPC 均不变。

## 替代方案

- 只在 `session:completed` / `session:error` 清理：拒绝。副事件可能在主事件发送失败后独立抵达，并且已负责终态 transcript 清理。
- 在两个 handler 无条件清理 Ask：拒绝。配对事件会重复 dispatch，并违反现有首个终态通道拥有 cleanup 的约定。
- 让 `session:updated` 永远不能做终态清理：拒绝。当前设计和回归已明确允许无 occurrence identity 的独立终态更新负责 UI cleanup。
- 修改 backend 以合并事件或增加新的 retry 通道：拒绝。现有 identity 与前端 first-wins 足以修复遗漏，不需扩展 wire contract。

## 影响与验证

- standalone terminal `session:updated` 会和主事件一样清理活跃会话 Ask；paired order 的两条通道仍只清理一次。
- inactive session 和 paused 等非终态事件不会触发 Ask 清理。
- 回归覆盖 completed/error standalone secondary、两种配对到达顺序及非终态隔离；Ask 内容和交互的实际 settle 语义由 ADR 0508 的 reducer/controller 回归覆盖。
- 无持久化或跨端契约变化，无需重置数据。
- 验证通过：`corepack pnpm run check`（0 error、0 warning）、`corepack pnpm run test:run`（122 files、981 tests）。

## 回滚

可回退 Ask 清理的 `shouldRunTerminalCleanup` guard 与 `session:updated` 清理分支，恢复只由 primary event 清 Ask 的实现；无 schema 或用户数据影响。若回滚，同步把本 ADR 标为撤销并在路线图注明该 secondary-only terminal gap。
