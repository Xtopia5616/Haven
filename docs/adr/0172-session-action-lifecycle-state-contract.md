# ADR 0172：会话与任务生命周期状态契约统一

## 状态

已接受（2026-09-19）

## 背景

Haven 的 session 和 action 都是可恢复、可取消、会向 UI 发出生命周期事件的实体，
但状态事实曾分散在多个层：Agent 自己定义 `SessionStatus`，Memory 用裸字符串；定时
任务在内存中是 `Waiting`，IPC 却称为 `scheduled`，数据库又用 `fired` 布尔值表示是否
已经触发。这样同一行数据可能同时被解释成“定时任务”“等待中”或“已触发”，恢复、取消、
历史查询和前端任务面板容易产生分叉。

## 决定

1. `haven-common::lifecycle` 是跨 crate 的生命周期词汇唯一来源：
   - `SessionStatus`：`Pending → Running ↔ Paused → Completed | Error`，终态只允许
     通过显式恢复策略回到可运行分支；
   - `ActionStatus`：`Waiting → Running → Completed | Failed | Cancelled`，定时任务
     在触发前也必须是 `Waiting`，取消和触发都进入终态。
2. Memory、Agent、ActionService、app IPC DTO 和 UI contract 只传递对应 typed 状态的
   canonical string。未知持久化值 fail-closed：session 映射为 `error`，action 映射为
   `failed`，不得因为坏数据重新排队或执行。
3. `ActionService` 是 action 状态的唯一运行时 owner；`kind=scheduled` 只表示任务类型，
   不再是状态。删除 `actions.fired`，待执行查询只看 `status=waiting`，触发/取消记录为
   `completed`/`cancelled` 并保留历史。
4. action 终态统一使用 `action:finished`；`action:updated` 只保留后台任务的会话归属
   更新。`list_action_history` 返回所有持久化终态，包括取消的定时任务。
5. 数据库 schema 直接升到 v22，不提供运行时迁移。旧数据库按
   `docs/release-and-reset.md` 删除并重建；这符合测试阶段的破坏性变更政策。

## 替代方案

- 保留 session/action 各自的字符串状态并增加校验：改动较小，但不能消除跨层词汇漂移。
- 继续用 `scheduled + fired` 表达定时任务：能兼容部分旧查询，却保留双重状态源和恢复
  分支，取消后的历史也无法自然表达。
- 为后台与定时任务保留两套 runtime registry：与 ADR 0155 冲突，会重新引入 owner、
  completion bus 和 session cleanup 的分叉。

## 影响

- 恢复、取消、历史和 IPC/UI 现在共享同一组状态名与迁移规则；定时任务的 pending/live
  与 terminal/history 边界可以由 `ActionStatus` 判断，不再依赖布尔列。
- session 的数据库行、actor watch、AgentEvent 和 Tauri `SessionLifecycleEvent` 使用同一
  `SessionStatus`，减少裸字符串比较；UI 仍在边界转换为 camelCase。
- 旧数据库、旧 action JSON 状态和依赖 `fired` 的脚本不兼容，必须重置；当前没有为旧数据
  编写隐式迁移，避免在恢复路径中混入不完整状态。

## 验证

- lifecycle transition table 单元测试覆盖未知值、终态不可复活和 action 取消；
- Memory action/session repository、ActionService、Tauri event projection、Agent session
  event 与 UI contract 测试覆盖 canonical statuses；
- 运行 `cargo fmt --all -- --check`、workspace check/clippy/test 以及 UI check/test/build；
- 运行 IPC event/contract 检查脚本。

## 回滚与重置

回滚代码时必须同时回滚 Common 状态枚举、ActionService、Memory schema、app DTO 和 UI
contract；不能只恢复其中一个层。由于 schema 已升至 v22，回滚到旧二进制前必须恢复一份
旧数据库备份或删除 v22 数据库文件后重建，不能让旧二进制打开 v22 数据库。
