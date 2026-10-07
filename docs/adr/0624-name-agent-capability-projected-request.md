# ADR 0624：命名 Agent capability-projected request

## 状态

已采纳并实施。

## 背景

`RequestContext::with_capabilities` 将当前 provider request context 按所选 adapter 的媒体能力重新投影，同时生成同一次决策对应的 `MediaPlan`，供请求执行和 UI/log 诊断使用。主 ReAct turn、compaction retry 和测试都按 tuple 位置读取这两个结果。request context 与 plan 必须来自同一 capability projection，错配会让实际发送内容与公布的媒体诊断不一致。

## 决定

1. 返回值命名为 `CapabilityProjectedRequest { request_context, media_plan }`，保留二者的同次投影关系。
2. 方法由 `with_capabilities` 改为 `project_for_capabilities`，明确这是 request-only 投影动作，而不是原对象上的 builder mutation。
3. 主 turn、compaction retry 与测试改为通过字段读取结果；媒体计划仍在流式请求启动前按原时序发布。
4. 保留 Arc 快速路径、media capability 决策、fallback 内容、canonical durable transcript 和 provider 行为。

## 替代方案

- 只给 tuple 解构绑定更长的局部变量：拒绝，配对关系仍无法由返回类型表达。
- 把 `MediaPlan` 合入 `RequestContext`：拒绝，RequestContext 表示 provider 可见的 transcript view；MediaPlan 是 adapter 投影诊断，且被独立发布给 UI/log。
- 改变媒体 plan 发布时机：拒绝，失败或取消前必须仍发布本次 request preparation 的结果。

## 影响与验证

- 这是 `haven-agent` crate 内部 Rust API 与方法名调整，没有 Tauri wire 或持久化变化。
- 命名审计 §5.7 保持 Active；其它 crate、UI、IPC 与配置/持久名继续逐域审计。
- 验证：Agent fmt、locked check、strict Clippy、crate tests、ADR 索引及 staged diff 检查。

## 回滚

恢复 `(RequestContext, MediaPlan)` 返回值和 `with_capabilities` 名称，并同步 turn、retry、tests 与路线图；无需数据或 wire 迁移。
