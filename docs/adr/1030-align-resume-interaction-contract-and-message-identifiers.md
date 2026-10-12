# 1030：统一 Session resume interaction 映射与消息身份命名

## 状态

已采纳并实施（2026-10-12）。

## 背景

UI 的 live interaction event 在 `contracts/app.ts` 中完成 snake_case 到 renderer camelCase 的映射；resume response 虽已由 `contracts/sessionHistory.ts` 校验身份，却又在 session reducer 中重复解析并映射。三个 resume 消费者都依赖该 reducer helper，边界职责因此不一致。

同一 transcript action 的乐观消息和持久消息字段则叫 `optimisticId`、`persistedId`，提交入口另称 `dbMsgId`；这些名字省略了所属实体。

## 决定

- 将 wire interaction 到 renderer `InteractionRequest` 的校验和映射集中到 `contracts/app.ts` 的 `mapInteractionRequest`，供 live event 与 Session resume 共用。
- `parseSessionResumeResponse` 先验证 resume interaction 的 session、owner、ask step linkage 和 wire shape，再返回已映射的 `InteractionRequest[]`。若快照中的 pending confirmation 没有有效 deadline，映射时省略该 interaction，保留既有 resume 行为和 transcript 数据。
- reducer 仅接收 typed renderer interaction，不再包含 wire DTO 的解析逻辑；startup、chat controller 与 Memory view 直接消费 Session command adapter 返回的映射结果。
- reducer action 的消息身份字段统一为 `optimisticMessageId`、`persistedMessageId`，提交入口变量统一为 `dbMessageId`，恢复关联集合统一为 `messageIds`。

## 替代方案

- 保留 reducer 内映射会继续让 reducer 依赖 Rust wire 字段，并与 live event 边界重复校验。
- 让每个 resume 消费者各自映射会复制相同 owner 和枚举规则，形成新的漂移入口。

## 影响

变更只调整 UI 内部类型与映射职责。Rust DTO、生成的 TypeScript wire contract、Tauri event/command shape、SQLite schema、durable event payload 与持久数据均不变。消息 ID 的值、resume interaction deadline 处理和 reducer 状态转换保持原有语义。

## 验证

- `corepack pnpm run check`
- `corepack pnpm run test:run`
- `corepack pnpm run build`
- `scripts/check-adr-index.ps1`
- `git diff --check`

## 回滚

若恢复 reducer helper，仍需保持 `contracts/app.ts` 为唯一 wire-to-renderer interaction mapper，并避免把 Rust DTO 字段解析重新分散到 startup、controller 或 view。
