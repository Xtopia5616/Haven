# ADR 1101：明确 Messaging 子会话名额按累计注册数计算

## 状态

已接受（2026-10-13）

## 背景

每个 parent 最多注册 8 个 child session。上限按 registry 中的 child 记录计数，包括 offline 和 ended child；原错误文案却建议“结束或复用”来释放名额，与实际累计计数行为矛盾。

## 决定

- 保持现有上限与累计注册行为，不改变 child 生命周期或 registry 语义。
- 错误明确说明 offline/ended child 仍计数，且不能通过结束现有 child 释放额外名额。
- child registry 的清除/谱系保留策略不属于本决定，需作为单独生命周期契约评估。

## 影响

用户和 Agent 不会再根据错误文案尝试无法生效的释放操作。上限触发后只能继续使用既有 child；新增 child 需要先有明确的 registry 生命周期策略。

## 验证

回归将一个 child 标为 offline 后填满上限，断言错误描述累计名额语义且不再建议“结束或复用”。

## 回滚

若产品需要删除 child 后释放名额，必须先定义 descendant authorization、历史消息、mailbox 和跨进程恢复如何处理，再连同计数行为和测试一起变更。
