# ADR 0184：运行中会话控制不阻塞界面

- Status: Accepted
- Date: 2026-09-20
- Owners: Haven maintainers

## 背景

工具执行期间会持续发送有界的 `agent:tool_output` 预览。此前每个预览
tick 都进入整个 `SessionReducer`，导致消息时间线和工具卡片投影反复重算，
主线程繁忙时折叠、停止输出和结束会话只能显示点击反馈而不能及时执行。

同时，`interrupt_session` 与 `end_session` 在返回前等待当前 run 完整退出。
工具或 provider 如果迟迟不响应取消，等待会持续到 run-exit 超时，UI 无法及时
得到控制结果。

## 决定

1. `agent:tool_output` 是 UI-only 的有界预览，不进入会话 reducer；前端通过
   独立的 preview store 更新当前工具卡片，并在 canonical observation 到达时清理。
2. `interrupt_session` 先取消 run token，再立即把会话置为 `paused` 并返回；
   `end_session` 先取消 token/后台 action，再立即把会话置为 `completed` 并返回。
3. 如果 run 仍在执行，terminal cleanup、partial promote、工具注册清理和 actor
   移除延迟到 dispatcher 的 run-exit 边界。terminal 状态阻止新的 run 被调度，
   因而迟到的工具输出不能复活旧会话或污染新的运行。
4. 删除会话和清空历史仍使用 cancel/dequeue/join 后再修改 registry/数据库的
   destructive cleanup fence；本 ADR 不放宽破坏性操作的等待语义。

## 影响

显式停止和结束不再被慢工具的退出速度阻塞，界面可以立即恢复操作。短时间内
允许出现“会话已暂停/结束但旧 run 仍在收尾”的进程内状态；该状态由 actor 的
run-exit 事件收口，不改变数据库 schema、IPC payload 或恢复权威事件流。

## 验证

- `cargo test --locked -p haven-agent end_session_returns_before_a_stuck_run_exits`
- `cargo test --locked -p haven-agent interrupt_session_returns_before_a_stuck_run_exits`
- `corepack pnpm --dir ui run test:run`
- `corepack pnpm --dir ui run check`

## 回滚 / 重置

回滚代码即可恢复旧的等待语义；不涉及数据库 schema、配置或历史数据重置。
