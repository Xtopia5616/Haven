# ADR 0464：删除项目中陈旧的兼容路径

## 状态

已采纳（2026-10-04）。

## 背景

代码仍保留数条没有当前生产输入的旧路径：Deepgram 同时接受 raw key 和两种带 scheme 的 secret；Memory 在删除 session 时清理旧 `fact_extraction_episode.<session_id>` 游标；恢复投影把缺失的 recovery `step_number` 当作 0；旧 compact-summary payload 可以从 event envelope 补回缺失的 `step_number`；ActionService 的单项状态与 session 列表 JSON wrapper 只在测试构建中暴露，测试仍经这些旧 wrapper 观察行为；前端 Settings contract 还允许 Rust 生成类型以外的任意字段通过；Inbox 只读操作会顺便重写旧版本留下的超大 archive。

## 决定

1. Deepgram 只接受原始 API key，并固定生成 `Authorization: Token <key>`。带空白的 scheme-prefixed 值作为配置错误拒绝；用户需要在模型设置重新输入原始 key。
2. 删除无生产 writer 的 `fact_extraction_episode.<session_id>` 旧游标清理分支。当前 episode 状态使用 `fact_extraction_episode_pending` / `fact_extraction_episode_done`。
3. committed recovery event 必须带 `step_number`；缺失时返回错误并回滚当前事务，不再以 step 0 静默 no-op。当前写入 API 总是要求 `u32 step_number`。
4. 删除仅供测试调用的 ActionService JSON status/list wrappers，测试直接读取 typed views，再按需检查工具边界序列化结果。
5. transcript compact-summary payload 必须包含 `step_number`；旧 payload 不再从 event envelope 恢复该字段。
6. 前端 Settings 读写类型严格引用生成的 Rust IPC 类型，不再接受额外的 `Record<string, unknown>` 字段。
7. Inbox 的只读操作只恢复中断的临时替换，不因旧 archive 超过大小上限而重写它；新归档写入仍执行限额。
8. 删除 `aggregate_stream_cancellable` 的测试专用 Vec 包装器；流式测试直接调用生产使用的 shared-snapshot 聚合入口。
9. schema v36 的旧数据库重置范围由 ADR 0463 统一定义。

## 替代方案

- 保留旧 key 前缀、游标和测试 wrapper：拒绝。它们没有当前生产写入者或调用者，继续保留会掩盖唯一有效输入格式。
- 缺少 recovery `step_number` 时默认 0：拒绝。损坏 marker 会被当成合法但无效的步骤，难以诊断并可能跳过应执行的修复。
- 用 event envelope 补齐 compact-summary payload：拒绝。事件 payload 是 transcript replay 的权威记录；当前写入者会写入该字段，旧数据库由 v36 重置边界处理。
- Settings 类型接受任意额外字段：拒绝。前后端 IPC 由同一应用版本构建，生成的类型是唯一契约；未知字段不应伪装成应用支持的数据。
- 读取时截断旧的大型 archive：拒绝。只读操作不得改写持久消息；archive 只在真实追加或恢复中断的文件替换时重写。
- 保留测试专用的 Vec 聚合入口：拒绝。测试应直接覆盖生产所用的 shared-snapshot 接口。
- 让 Deepgram 静默把 scheme-prefixed secret 修成 raw key：拒绝。配置边界要求存储原始凭据，非法输入应明确失败。

## 影响与验证

Deepgram 凭据需要使用 raw key；旧 episode cursor 不再由维护/删除操作清理；不完整的 committed recovery marker 显式报错；旧 compact-summary payload 会作为损坏 event 拒绝回放；ActionService 与 LLM 流式测试直接使用 typed/shared 入口；Settings 前端类型只接受生成契约内的字段；超大 archive 仅在有真实追加时压缩，读取不会截断历史文件。普通生产 action/tool JSON shape 不变。v36 重置会清掉旧游标与旧 transcript 数据。

验证：更新既有 Memory、Deepgram、ActionService、transcript replay、Inbox 与流式测试夹具及断言；静态搜索确认生产代码无旧 cursor writer，测试无旧 wrapper 调用。目标 crate `cargo check` 与严格 Clippy、workspace 格式检查、`git diff --check` 和 `corepack pnpm run check` 通过。UI 检查使用 Node 24.18.0，低于仓库固定的 24.20.0；本轮未运行测试或生产构建。

## 回滚

代码可按本 ADR 反向恢复；已清理的旧 cursor 不恢复。v36 数据库需在恢复 v35 二进制前重建或恢复 v35 备份；Deepgram 用户需在设置中输入兼容旧格式，除非一并回滚旧 key 校验。
