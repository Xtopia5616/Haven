# ADR 0428：会话确认结束时发布恢复状态

- 状态：Accepted
- 日期：2026-10-02
- 关联：ADR 0200、0423、0424

## 背景

最后一个工具确认获批后，`SessionSupervisor` 已将暂停会话转为 `pending` 并唤醒
dispatcher，但没有把该生命周期变化投影给 renderer。前端仍保留确认前的
`paused` + `confirmation`，显示为“等待操作”，直到后续 ReAct 事件恰好更新会话。

## 决定

1. 最后一个确认实际成功地将会话从 `paused` 转为 `pending` 时，supervisor 发布 typed
   `SessionResumed` 事件；状态未改变时不发布。
2. `AgentLayer` 将该事件投影为现有的 `session:updated`，状态为 `pending`，等待原因为
   `null`。投影先于 dispatcher 唤醒，因此前端能及时清除已结束的确认等待状态。
3. scheduled 和 UI 直调确认不暂停所属会话，因此不产生此事件。

## 替代方案

- 让 renderer 在点击确认后猜测会话已恢复，会在确认批次尚有其他待决请求时过早清除等待状态。
- 新增 Tauri channel 或查询命令会复制现有生命周期契约；沿用 `session:updated` 即可。

## 影响与重置

新增的 `SessionResumed` 只存在于进程内 supervisor 事件流；Tauri payload、数据库 schema、
transcript 与授权语义均不变。现有 `session_resumed` 通知设置继续适用。无需重置用户数据。

## 验证

回归覆盖应确认：非最后一个确认不唤醒或发布恢复状态；最后一个确认只在成功转换后发布一次；
renderer 收到后清除 `waiting_reason`。运行项目规定的 Rust、IPC 与 UI 门禁。
