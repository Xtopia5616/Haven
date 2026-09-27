# ADR 0380：收紧前端 event 枚举校验

- 状态：已采纳（2026-09-27）
- 范围：前端 session/action/app/agent event mapper
- 关联：[ADR 0330](0330-session-lifecycle-ui-contract-mapper.md)、[ADR 0335](0335-action-board-ui-contract-mapper.md)、[ADR 0346](0346-app-event-listener-contract-boundary.md)、[ADR 0347](0347-agent-event-contract-validation-boundary.md)

## 背景

ADR 0330、0335、0346 与 0347 曾将未知 session/action status、waiting reason、交互/MCP status 和工具分类降级或透传。应用与 renderer 按同一版本发布，这些字段不是独立扩展点；未知值被映射成看似有效的状态，会让消费代码在 UI 边界之后才遇到不完整状态。

## 决定

1. `mapSessionEvent` 对 session status 与 waiting reason 使用当前值集校验；省略的 waiting reason 映射为 `null`，显式 null 或未知值丢弃事件。`reason` 省略映射为 `null`，Rust DTO 未声明的显式 null 不接受。
2. `mapActionPayload` 对存在的 action status 使用当前值集校验；Rust DTO 省略的 Option 字段若显式为 null 或未知值，丢弃 row/event，不再伪装成 `failed`。
3. `mapAppEvent` 对 bootstrap/tray status、Skills operation (`refresh` / `auto_refresh` / `toggle`)、interaction kind/status/risk level 使用当前值集校验；MCP status 只接受三个当前 unit variants 或 `{ Offline: { error } }` 当前形状，并只投影已知字段。
4. `mapAgentEvent` 对 observation/tool result outcome、error class、retry safety、retryability 与 operation scope 使用当前值集校验；未知值与其他畸形 payload 一样返回 `null`，由现有 listener 记录不含 payload 的 warning 并丢弃。
5. 对 interaction `Option` 字段只接受省略或当前值，不接受 null；保留 Rust DTO 省略空 interaction options 时的 `[]` 默认。保留明确的动态扩展字段与未知附加字段忽略行为；不改 producer、channel、副作用顺序、Rust wire DTO 或全局 codegen。

## 影响、重置与验证

- 只收紧前端 event mapper 的输入校验；没有持久化、配置、IPC wire 或 schema 变化，不需要用户数据重置。
- 回归测试覆盖未知 enum 值拒绝和当前值映射；运行 UI check、全量 UI tests、production build 及 IPC contract scripts。

## 回滚

恢复 ADR 0330/0335/0346/0347 中未知 enum/status 降级、透传及显式 null 容忍分支与对应测试；无需数据迁移。
