# ADR 0837：通过 Session 命令 owner 调用会话生命周期

## 状态

Accepted — 2026-10-09

## 背景

`sessionCommands.ts` 已包装会话列表、恢复、历史和删除命令，但 `chatSessionController.ts` 仍通过通用 `TauriCommandInvoke` 直接调用 `rollback_session`、`end_session`、`interrupt_session` 与 `continue_session`。Controller 同时把同一通用 invoke 传给 `getSessionForResume`。这使 controller 获得了所有 Tauri 命令的能力，也让 Session 域的 command adapter 只覆盖部分会话操作。

## 决定

- 将 rollback、end、interrupt、continue 四个 Session 生命周期命令加入 `sessionCommands.ts`，与恢复读取、历史及删除命令共用唯一 Tauri invoke owner。
- 删除 `getSessionForResume` 的可注入通用 invoke 参数；该 adapter 自己读取其唯一底层命令。
- `ChatSessionController` 只接收由 `sessionCommands.ts` 派生的受限 command port，不再持有通用 `TauriCommandInvoke`。
- `+page.svelte` 通过显式方法对象装配 controller；controller 仍拥有请求期间的锁、UI 状态转换、错误处理和通知，不把页面行为移入 adapter。
- IPC owner 检查清单改为 `sessionCommands.ts` 当前实际拥有的命令，删除已经不存在的旧 history 命令名，并阻止生命周期命令旁路。

## 替代方案

- 让 controller 继续持有通用 invoke：拒绝。它只需要五个 Session 命令，不应获得全量 Tauri IPC 能力。
- 把生命周期请求并入 reducer：拒绝。reducer 只投影命令结果，不应成为副作用 owner。
- 在 `chatSessionController` 保留局部命令函数：拒绝。会与已有 `sessionCommands.ts` 形成并列 Session command owner。

## 影响与验证

仅改变 UI 内部命令所有权与依赖能力，不改变命令名、wire payload、持久化、rollback/end 顺序、错误语义或用户可见交互，无需数据重置。验证：UI 类型检查、Session command/controller 测试、UI 全量测试与生产构建、IPC contract 检查、Prettier、ADR 索引和差异检查。

## 回滚

恢复 controller 的通用 invoke dependency、恢复 `getSessionForResume` 的 injected invoker 参数，并移除四个 Session command wrapper 与 IPC owner 清单更新；无需数据库或配置重置。
