# ADR 0805：关联 interaction kind 与 owner

## 状态

已采纳并实施（2026-10-08）。

## 背景

交互生产路径具有固定 owner：Ask 由 Session 持有；普通确认由 Session 或 AppCommand 持有；定时确认由 ScheduledToolRun 持有。此前 renderer 的 `InteractionRequest` 只区分 owner，没有把 `kind` 与 owner 关联；event/resume mapper 接受生产者不会生成的组合，通用 `interaction-resolved` action 也允许把非 Ask 请求标为 Ask 已解决。

## 决定

- 将 renderer request 类型收窄为上述四种 kind-owner 组合；只有 Ask 分支可以带 renderer-local `AskResponseView`。
- app event mapper 与 resume mapper 共用 kind-owner runtime 校验，丢弃不匹配的 payload。
- 把 reducer action 命名为 `session/ask-resolved`，要求提供 response，并且仅解析 pending 的 Session Ask。

## 影响与回滚

Rust producer、Tauri wire 字段、持久化格式与合法交互行为不变。UI 会拒绝不符合现有 producer owner 路由的 malformed event/resume 行；不涉及数据迁移或重置。若未来 producer 增加 owner 路由，应先更新此 view contract、两处 mapper 和对应反例测试。

## 验收

运行 UI 类型检查、完整 UI 测试和 ADR 索引检查；测试覆盖各合法路由、不匹配的 event/resume payload，以及 confirmation 不受 Ask reducer action 影响。
