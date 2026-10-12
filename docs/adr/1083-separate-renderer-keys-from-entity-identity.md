# ADR 1083：分离 renderer 展示键与实体身份

## 状态

已接受（2026-10-13）

## 背景

补充事件的 `supplement_id` 是 canonical `msg-*` 身份；跨会话 inbox 与 ToolRun 结果需要以工具卡形式呈现。ToolRun 结果同时可能有同 ID 的持久用户消息行。把派生的 `peer-mail-*` / `tool-run-result-*` 写进 `SessionMessage.id` 会引入未经登记的实体 ID 格式；直接复用 ID 又会让同一实体的第二个 renderer 投影在 resume 合并和列表 keyed rendering 中被折叠。

此外，聊天模型选项中的泛名 `id` 实际是 ModelConfig 身份，`currentModelId` 也未指明 owner；旧 `haven` 名称回退会把未知工具渲染成管理面板。ask/confirm 的 dismissed 集合还会在请求终态后保留不再使用的请求 ID。

## 决定

- `StreamMessage.id` 继续承载 canonical 实体身份；需要同一实体的额外 renderer 投影时，使用稳定 `presentationKey` 区分可见列表项。该 key 只用于渲染和投影合并，不参与领域关联，也不作为持久实体 ID。
- Peer inbox 与 ToolRun 结果卡复用经过边界校验的 `supplementId` 作为实体身份，并分别设置 `peer-mail:` / `tool-run-result:` presentation key；renderer keyed each 与 resume merge 按展示键保留投影，实体关系仍按 `id`。
- 聊天模型配置字段和状态统一命名为 `modelConfigId` / `currentModelConfigId`；设置 DTO 自身的 Rust-owned `model.id` 保持原字段，在 UI 适配时转换。
- 未知或旧 `haven` 工具名使用通用标题和图标，不隐式解释为管理能力。Ask、Confirm 与 ScheduledConfirm 的 dismiss 集合只保留仍 pending 的 request ID。

## 影响

改变 renderer 进程内消息 view、聊天模型状态和临时交互集合；不改变 Tauri wire contract、持久消息 ID 或 schema。额外 renderer 投影可以与同一 `msg-*` 实体的 transcript 行并存，但必须拥有不同 `presentationKey`。

## 验证

- reducer tests 验证补充事件使用 canonical ID、独立展示键并按事件身份幂等。
- resume merge tests 验证同一实体的用户消息和工具卡都被保留且有不同 renderer key。
- UI identity、model toolbar 与 interaction lifecycle tests，加 UI type check 和完整 UI tests。

## 回滚

若移除 `presentationKey`，必须先保证补充工具卡不再与 canonical transcript 行并存，并同步恢复 resume 合并、Svelte 列表 key、相关 reducer 和测试；不能把 renderer key 改回未经登记的 `SessionMessage.id` 格式。
