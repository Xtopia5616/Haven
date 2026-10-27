# ADR 1434：结束会话失败按用户选择隔离

## 状态

已实施 — 2026-10-27。

## 背景与证据

`ChatSessionController.endSession` 在发起结束前设置 fresh intent，防止生命周期事件自动选择已有会话；命令失败时却无条件清除该标记。等待期间，页面 `newSession` 可能已经递增用户选择版本、设置新的 fresh intent 并清空活动会话。旧请求的失败会覆盖这一新意图，令后续 created 事件或列表刷新重新采用已有会话。

## 决定与不变量

- 结束命令仍绑定发起时的会话，不因用户切换而改目标或取消后端结束。
- 失败恢复只有在发起时的用户选择版本仍当前、活动会话仍为原会话时才能清除 fresh intent；切走再切回同一 ID 也属于新选择。
- 同一选择下的失败继续保留活动会话并恢复正常输入，错误仍可见。
- 页面、reducer 和 fresh-intent owner 不变；不新增影子意图或按内容去重。

## 替代方案

只比较 session ID 无法隔离切走再切回；失败后永不恢复会使同选择下的失败保留错误意图。新建全局动作状态机超出该已复现竞态的最小范围。

## 影响、回滚与重置

仅修改 UI 内部失败恢复的选择边界；无 Rust、IPC、数据库、配置或持久事件变化，无需重置。回滚恢复无条件清除及配套测试。结束成功后的生命周期处理和 interrupt 的展示隔离不在本切片改动范围。

controller 的测试文件超过职责复核阈值，本次保留共同的 session action harness，并通过真实 event/startup owner 验证选择边界；单独拆分这四项会复制同一夹具，不形成新的生产职责或测试隔离收益。

## 验证

- 先运行旧实现的回归：旧 End 拒绝后，真实 created handler 将活动指针选为新 session，期望为 null；1 项失败、44 项过滤后才修复生产路径。
- 新增回归覆盖新草稿后的 created 事件、真实列表刷新、真实切走再切回后再次 End，以及终态已清活动指针；原同选择失败的恢复测试保留。
- Node 24.21.0、pnpm 12.12.1。
- 定向 `vitest run src/lib/chatSessionController.test.ts src/lib/chatSessionStartup.test.ts src/lib/chatSessionEventHandlers.test.ts --maxWorkers=2 --reporter=dot`：3 文件、72 项通过。
- `corepack pnpm --dir ui run check`：0 errors、0 warnings。
- `corepack pnpm --dir ui run test:run --maxWorkers=4 --reporter=dot`：170 文件、1536 项通过。
- `corepack pnpm --dir ui run build`：通过，CSP manifest 生成完成；未生成 Tauri 发布包。
- 受影响文件 Prettier、ADR index（1331 条）和 diff 检查通过。
- 仅 UI 内部变更，未重跑 Rust/workspace 门禁；本轮先前 Tools/Agent 严格验收见 ADR 1433。真实 WebView 并发验收仍由发布 Gate 跟踪。
