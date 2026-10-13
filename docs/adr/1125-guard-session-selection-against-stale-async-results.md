# ADR 1125：用用户选择代次隔离过期异步会话结果

## 背景

切换历史会话可能先异步 reopen，再异步读取 resume projection；较早请求晚于较新的用户选择完成时，旧结果仍可能写入消息投影并重新选中旧会话。正在执行的 transcript submit 也可能在用户切换会话或点击“新会话”后返回 `SessionCreated`，从而覆盖新的选择。提交层与页面 controller 都有自动选择入口，因此只在单层检查不足以覆盖 UI 和语音提交。

## 决定

1. `SessionReducer` 持有只在当前进程有效的用户选择代次。页面开始切换会话时、在任何 reopen/load await 之前递增；点击“新会话”同样递增。
2. controller 将切换代次传给 resume 读取。只有代次仍为最新时才物化 resume projection、更新选中会话或发出切换通知；历史会话重开的旧结果也不继续进入后续加载。
3. `submitTranscript` 在提交被接受时捕获用户选择代次。`SessionCreated` 仍迁移并确认该提交的乐观消息，但仅当代次未改变时自动选中新会话。controller 的返回结果选择也执行相同检查，覆盖 mock/替代提交实现。
4. 继续和回退命令仍作用于各自开始时捕获的 session；若用户中途切换，完成后的继续重提和回退草稿恢复/提示不再作用于新选中的会话。
5. 此代次不持久化，不改变 IPC、事件或提交 lane 的 session pin、FIFO 和显式 submission token 语义。

## 替代方案

- 仅在 resume 完成后比较 active session：切换页面在 resume 前尚未选中目标，且无法识别新一轮选择意图。
- 只检查 controller：`submitTranscript` 和 voice submit 可直接选中新建会话。
- 尝试取消旧 reopen/resume/submit：这些 command 没有统一可取消契约；让其完成并丢弃过期的 renderer 状态写入更可靠。

## 影响

过期 resume 不再覆盖 reducer 投影或改变当前会话；较早提交创建的会话及其消息仍然保留，但不会抢走更新的用户选择。该代次在应用重启后重置，不要求数据库迁移或重置。

## 验证

- 测试覆盖较早 resume 晚返回时不写 projection、不选择会话。
- controller 与 submit 测试覆盖新选择开始后，旧 `SessionCreated` 不覆盖当前会话，同时其消息仍完成迁移；继续不会向新会话提交旧输入，回退不会改写新会话草稿。
- 运行 `corepack pnpm run check` 和 `corepack pnpm run test:run`。

## 回滚

回滚只恢复异步完成时无条件选中的行为，不需要数据迁移。用户仍可手动切回目标会话。
