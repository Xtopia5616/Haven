# ADR 0012：Agent 恢复、回滚与 Hook 策略边界

## 背景

P2 的第一刀已经把回合结束和待处理上下文拆出，但恢复流程仍同时包含
生命周期切换、快照门禁、未投递输入扫描、无快照投影和运行时工具重建；回滚
流程也把事件日志操作和数据库时间线截断混在一起。Hook 接口、生产策略和
测试替身则集中在同一文件，导致测试容易间接触达 inbox、压缩和推理副作用。

## 决定

1. `resume.rs` 只编排恢复生命周期和 ReAct 运行；`resume_support.rs` 提供可
   确定测试的恢复候选合并、MCP 工具选择解码，以及无快照时的
   `session_steps` → canonical 投影。只有缺失 `react_state` 时才允许使用这条
   有损投影路径；损坏或不可读快照继续硬失败。
2. `reopen_session` 与运行时 skill/MCP 重建归入恢复边界，不再由通用
   `layer.rs` 承担。运行时工具注册是派生缓存，来源始终是恢复后 events 的
   投影结果，不成为第二个会话真源。
3. `rollback_support.rs` 只处理纯事件时间线操作。回滚删除新事件时优先匹配
   `UserInject.message_id`；只有无 ID 的旧事件或压缩前无法保留 ID 的 canonical
   用户行才按内容兜底。数据库截断、运行取消/等待和状态迁移仍由
   `rollback.rs` 编排。
4. `hooks.rs` 只保留 `LoopHooks` 契约、输入类型和 `NoopHooks`；生产策略
   （inbox/compact/infer、响应策略和安全门）位于 `hook_policy.rs`。循环依赖的
   Hook 仍只通过契约调用，默认生产装配不改变。
5. 全部路径继续遵守 X12：`ReActSnapshot.events` 是恢复唯一权威，
   `messages`/`session_steps` 只能作为物化投影或明确标记的缺失快照恢复来源；
   不引入按内容去重。

## 替代方案

- 继续在 `AgentLayer`/`rollback.rs` 中堆叠 helper：短期改动少，但会让 DB、
  生命周期和纯事件算法无法分别验证，拒绝。
- 让回滚始终按文本寻找用户消息：重复输入时可能删除错误分支，拒绝。
- 为每个副作用再建一套 loop 调用路径：会复制顺序契约并扩大 side effect
  面，拒绝。

## 影响

- 新增 `resume_support.rs`、`rollback_support.rs` 和 `react/hook_policy.rs`；
  对外 Agent API、快照 JSON、数据库 schema、配置和 IPC 不变。
- 新消息的回滚身份由持久化消息 ID 决定，重复文本不再影响选择；旧的无 ID
  快照保留有限内容兜底。
- 不需要删除数据库、配置或缓存；恢复/回滚失败仍应按发布说明清理不兼容的
  `react_state`。

## 验证

```text
cargo fmt --all -- --check
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace -- --test-threads=1
pnpm --dir ui run check; pnpm --dir ui run test:run; pnpm --dir ui run build
```

重点回归恢复候选按 ID 去重、无快照工具链投影、消息 ID 优先的回滚、旧快照
内容兜底、dangling tool call 清理以及 NoopHooks 不触达维护副作用。

## 回滚与重置

代码回滚时恢复 `AgentLayer` 中的恢复扫描/工具重建、`rollback.rs` 中的事件
操作和 `hooks.rs` 中的生产实现即可；不需要数据库或配置重置。若快照本身已
来自不兼容版本，仍按 `docs/release-and-reset.md` 清理数据根目录，而不是混用
新旧快照。
