# ADR 1431：派生会话的创建结果与页面协调分层

## 状态

已实施 — 2026-10-27。

## 背景与证据

R12 的切换、rollback 和提交已按 ADR 1415 返回具名结果，但 `ChatSessionController.forkSessionFromMessage` 仍将创建、列表刷新、选择、通知与弹窗关闭放在同一个异常边界。如果后续阶段抛出异常，会被统一归为“创建派生会话失败”，无法表达已经创建的子会话身份。旧请求的 finally 还会关闭后来打开的派生弹窗。

进一步核对生产调用发现，`ChatSessionStartup.loadSessionsNow` 会捕获列表请求错误并正常完成 Promise，原 controller 的刷新异常分支以及 ADR 1415 的页面刷新结果分支均无法感知这类失败。需要从列表 owner 提供显式操作可读取的失败结果；仅在测试中注入会拒绝的刷新替身不足以验证真实链路。

## 决定与不变量

1. 创建命令返回具名结果，区分没有活动会话、重复请求、消息尚未持久化、创建失败和创建成功。成功携带子会话身份及发起时的来源会话和用户选择版本。
2. 页面拥有列表刷新、选择协调、用户通知与弹窗关闭。创建成功后，刷新或切换失败不能改写创建结果；刷新失败仍可尝试加载并选择已创建的子会话。
3. 异步刷新和切换沿用同一个选择版本。用户切换、新建或切走再切回后，旧创建结果不能重新选择子会话。
4. controller 保留创建命令的串行准入和加载状态；页面覆盖整个协调过程的重入防护。关闭弹窗只作用于发起请求时捕获的弹窗实例，不能关闭后来打开的弹窗。
5. 列表 owner 提供 `refreshSessionListForAction`，使提交、rollback 和派生操作收到真实查询拒绝；`loadSessions` 保持安全入口。两者复用同一 scheduler，scheduled/queued 后台工作捕获拒绝，WeakSet 按请求身份去重报告及标记显式操作的报告归属，初始化等待始终使用 settled Promise。

## 替代方案

- 扩大创建命令的 try/catch，统一报告失败：仍会将已完成的持久副作用误报为失败。
- 用通用 action bus 或新全局 store 统管动作：没有必要；复用现有 controller、reducer 选择版本和页面编排边界。
- 保留 controller 的页面回调：无法让调用方可靠区分每阶段结果，也继续耦合弹窗生命周期。

## 影响、重置与回滚

仅改变 UI 内部动作结果和页面副作用归属；Rust、IPC、数据库、配置与 durable event 均不变，无需重置。用户可区分派生会话创建失败和创建后的列表/切换失败。回滚时恢复 controller 与页面调用链，不触碰已创建的会话数据。

`+page.svelte` 已超过职责复核阈值，本切片保留它作为弹窗和通知的展示协调 owner；异步阶段组合与可测试结果放入现有 `chatSessionOperationResults`，选择和投影仍归既有 controller/reducer，不再提取只转发的页面 helper。

## 验证

- 固定工具链：Node 24.21.0（官方 ZIP 与 SHA-256 校验）、pnpm 12.12.1。
- 定向 `vitest run src/lib/chatSessionController.test.ts src/lib/chatSessionStartup.test.ts src/lib/sessionRefresh.test.ts --maxWorkers=2 --reporter=dot`：3 文件、59 项通过；覆盖真实列表拒绝的结果传播、后台拒绝、重复创建、切走切回和迟到恢复成功/失败。
- `corepack pnpm --dir ui run check`：0 errors、0 warnings。
- `corepack pnpm --dir ui run test:run --maxWorkers=4 --reporter=dot`：170 文件、1532 项通过；有既有 `TimeoutNaNWarning` 和 `IndependentSearchSettings` 非 reactive binding 提示，无失败。
- `corepack pnpm --dir ui run build`：通过，CSP manifest 已生成；未生成 Tauri 发布包。
- 受影响 TypeScript/Svelte 文件 Prettier 检查通过；ADR index 与 `git diff --check` 通过。
- 弹窗实例身份保护由页面代码复核，尚无组件级自动测试；真实 WebView 并发交互仍按路线图 §5.1 跟踪。本切片无 Rust/IPC 变化，未运行 Rust workspace 门禁。
