# ADR 0842：固定 Route 与 App shell 的命令 owner

## 状态

Accepted — 2026-10-10

## 背景

前端命令所有权复核已为 feature adapter 建立 IPC guard，但 Route/App shell 的跨域应用生命周期命令只对 `check_llm_connection` 做了专门断言。当前 `+layout.svelte` 还直接执行启动 readiness 查询与全局 confirmation resolution；这些调用各自拥有完整的 shell 协调流程，并没有第二个调用方。提交、全局错误日志、外链和录音命令也分别由 `submit.ts`、`errorHandling.ts`、`externalRef.ts` 与带窄命令端口的 `recordingOverlayController` 所有。

命令本身都受 generated contract 约束；缺口在 owner 守卫没有完整登记这些直接调用，容易让未来 feature 页面增加绕过路径。

## 决定

- 将 `get_bootstrap_status`、`check_llm_connection` 与 `resolve_confirmation` 登记为 `+layout.svelte` 的 shell owner，并断言其启动、generation gating 与 request/result 路径仍在该 owner。
- 将提交、错误日志、外链与录音 lifecycle invoke 分别登记到现有唯一协调者/策略 owner；录音的动态 invoke 只允许通过 `RecordingCommandName` 窄端口。
- IPC guard 同时阻止其他 UI 文件发出已登记命令的 literal invoke，并拒绝 recording controller 之外的 `invoke(command|cmd)` 动态旁路。
- 保留 Route/App shell 自身发起上述唯一调用的结构，不为无重复调用的生命周期再增加转发 wrapper；feature view 与组件继续通过领域 adapter/callback 发起动作。

## 替代方案

- 为每个 shell 命令创建单行 adapter：拒绝。当前命令各只有一个消费者，adapter 不会增加校验、复用或能力收窄，只会把 shell 生命周期拆成额外跳转。
- 允许 shell 与页面自由直接 invoke：拒绝。直接调用的例外必须能从 owner map 与门禁断言中核对。

## 影响与验证

仅加强 UI 内部 owner 文档和 IPC 结构守卫，不改变命令名、request/response、运行时调用顺序或持久数据，无需重置。验证使用 `check-ipc-contracts.ps1`、ADR 索引检查与差异检查。

## 回滚

若命令消费者/生命周期迁移，更新 owner map 和 assertion 到新唯一 owner；不得移除唯一性检查后恢复未登记的 route/view 直调。
