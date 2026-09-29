# ADR 0351：配置 runtime apply 失败语义审计

- 状态：已采纳（2026-09-25）
- 范围：Phase 5 的 Settings/model apply 所有权、失败、重试与补偿语义
- 关联：[ADR 0235](0235-versioned-runtime-config-apply-boundary.md)、[ADR 0253](0253-runtime-config-coordinator.md)、[ADR 0323](0323-model-config-apply-coordinator-ownership.md)、[ADR 0324](0324-settings-apply-phase-failure-observability.md)、[ADR 0337](0337-settings-runtime-apply-planning.md)

## 审计结论

Phase 5 的局部 target、phase 顺序和 Settings 失败观测没有第二份生产实现：`RuntimeConfigApplyPlan` 是 config domain → runtime target 的共享映射；`SettingsApplyPlan` 从它派生 targets，并从一份固定 phase 序列筛选有序步骤；`SettingsRuntimeApplyCoordinator` 是 Settings phase、停止后续执行、Router published 状态和结构化失败/警告记录的唯一 owner。`SettingsApplyOutcome` 已是 typed callback 结果，`SettingsApplyObservation` 已是 typed 诊断上下文。另抽一层通用 failure report 或 plan-validation policy 会重复这些职责，不能消除实际 owner 边界；本切片不增加该层。

`RuntimeConfigCoordinator` 是 Settings 和 model 共用的 gate 与 Router/media prepare/publish owner。model 命令由它执行 durable edit；`update_settings` 仍在 Settings 命令中用一次 `ConfigService::edit` 捕获旧 hotkey、持久化 settings 并取得同一 snapshot，再把 runtime phase 交给 Settings coordinator。两个入口持有的是同一 gate，不是两把锁。Router/media builder、Tools `PlatformRuntime`、MCP manager、Skills、日志 filter 和 hotkey 仍分别是具体副作用 owner。

| 范围 | 当前权威 | 审计结果 |
|---|---|---|
| Domain target | `RuntimeConfigApplyPlan::from_change` | Settings 与 model 共用；没有第二套 domain 映射 |
| Settings phase 顺序 | `SETTINGS_APPLY_PHASE_ORDER` + `SettingsApplyPlan` | 单一静态顺序；prepare 在 publish 前，失败立即停止后续 phase |
| Settings failure state | `SettingsRuntimeApplyCoordinator` | phase、snapshot version、Router published 与 restart-required targets 集中维护 |
| Error rendering | `log_err` 与 `SettingsApplyObservation::record` | 命令错误 renderer 保持既有文本；phase metadata 由 coordinator 记录。prepare builder 已渲染的错误仍走专用标记，避免重复执行 `log_err` |
| Retry/compensation | 无 Settings/model apply retry 或 compensation owner | MCP 自身连接重试及 admin operation 的局部恢复属于不同操作，不是 Settings coordinator 的第二份策略 |

## 失败与可重试边界

- `ConfigService::edit` 保存新配置后递增内存 version 并发布 `ConfigChanged`，随后才进入 runtime apply。Settings 或 model 的 Router prepare 失败不会撤销配置。Settings 对同一 payload 再调用时是 no-op，不会重跑 apply；model 同一 mutation 再调用时 Router target 不再触发。model command 原有成功路径仍会发送 `llm:config_changed`，该事件不能重试 Router apply。当前没有针对已保存 snapshot 的显式 retry command。
- Router/media prepare 在 publish 前构造替换对象；失败时 Settings 后续 phase 不运行，model 的 Router/media live generation 也保持旧值。准备对象可丢弃，按同一 snapshot 再构造是最清晰的局部重试候选，但现有命令没有暴露这个重试入口。
- Router publish 先替换 Agent Router，再经 Tools runtime 更新 Router/media platform 并重建相关 catalog；它没有共同原子交换。Settings coordinator 现在能收到 Agent Router 锁失败和 catalog rebuild 拒绝：前者发生在 Agent Router 替换前，后者发生在替换后并记录 `router_published=true`。后者仍发送 `llm:config_changed`，随后停止 Settings 后续 phase；不恢复已替换的 Router generation。
- Security apply 清除 session grants 并递增 policy revision。MCP config apply 会更新 server config index、catalog generation、连接/断开客户端并发布 status；MCP monitor phase 会为当前 client 启动 monitor task。重复执行整段 phase 可能清除临时授权、产生连接和事件副作用或启动重复 monitor，不能视为安全重试。MCP client 的断线重连由 MCP manager/client monitor 自己拥有。
- Skills `set_config` 先替换 root/allowlist，再扫描目录；扫描失败时新 root 已保存到 engine，而旧 skill map 可能仍在。Logging 逐个修改 reload handle，失败时此前成功的 handle 不回滚。Hotkey unregister 成功而 register 失败会留下旧快捷键已移除的状态，不恢复旧绑定。它们都发生在 durable edit 之后，后续 Settings phase 停止，已完成状态保留。
- Settings phase 的 apply 错误现在按 owner 传播：shell、context、tool settings 和 Router publish 的 builtin catalog rebuild 拒绝会到达 coordinator；Agent 的 media strategy、context limits、session step limit 与 router mutex poisoning 也会返回错误。InputPipeline 的配置替换、context pipeline limit 更新与 executor 并发上限更新没有内部失败分支，不制造虚假错误。Security 与 MCP config/monitor 接口仍由对应 owner 通过 MCP status/warning 表达连接和 monitor 结果，不把外部连接状态伪装成 Settings command 的 fatal apply error。Hotkey rebind event 仍是 warning-only。
- 以上局部 setter 有些在相同输入下可重复写入，但各 phase 可能跨多个 runtime owner，且会重建 catalog、清理临时授权、启动任务或连接外部 MCP server；本 ADR 不把它们提升为可安全重试契约，也不新增补偿。

因此只有尚未 publish 的 Router/media prepare 可作为“保留同一 snapshot 后重新构造”的安全候选；当前 command 没有保留该 retry context。live phase 没有 phase-level retry API。已保存配置可由用户后续修改覆盖，但这不等同于撤销已发的 `ConfigChanged`、已发布的 live runtime 或外部 MCP/hotkey 副作用。

## 实现跟进（2026-09-27）

已按上节产品决定落实：Settings runtime phase 与 model Router apply 在 durable edit 后失败时返回“部分 apply 失败”，保留已写入的配置、停止后续 phase，且不自动 retry 或 compensation；SettingsView 告知用户配置已写入以及重启后重新初始化。启动仍从 `ConfigLoader` 读取配置。保存成功的保证范围及断电持久性限制见 ADR 0372。

Tools AdminServices 的 config writers 现在与 Settings/model 共用由 app composition root 创建的 gate，锁覆盖 edit 和其 live apply/rebuild。gate 经窄 `AdminContext` 注入，不改变 app→Tools 依赖方向。Settings/model 之间及 Tools admin writer 与 Tauri apply 之间的互斥由回归测试覆盖。SkillsExec-only 计划没有 live Skills phase；混合 Skills 与 SkillsExec 允许 Skills 同时出现在 live 和 restart-required 集合，既有 phase 顺序和 owner 不变。

## 并行配置写入口（审计时状态，2026-09-25）

在本 ADR 审计时，`RuntimeConfigCoordinator` 的 gate 只覆盖 Settings 与 model edit+apply。Tools admin surface 另有修改相同配置域并调用 runtime port 的操作：`logs_level`（Log/Logging）、`tool_set`（Tools/ToolSettings）、`skill_set` / `skill_create`（Skills/Skills）和 MCP add/update/toggle/remove；`mcp_reload` 不改配置，但直接重建 live MCP state。`skill_set` 在持久化失败时恢复启用位，`skill_create` 在持久化失败时尝试删除新建目录并刷新 skill catalog；MCP update 在持久化前尝试新连接、失败时恢复旧连接，MCP update 保存失败后也尝试移除新连接并恢复旧连接。这些 operation-specific 局部补偿与 Settings 的先持久化、后 apply、失败不回滚语义不同。Logging/tool toggle 的 admin 路径也有自己的持久化与 runtime 顺序。永久权限写入口单独维护 permission grant；`AppConfig::apply_settings` 保留权限列表，避免 Settings 表单覆盖它。2026-09-27 实现跟进已让这些 AdminServices writer、Settings/model 和永久权限 Tauri writer 共用组合根创建的 gate；AdminServices 在 service 内覆盖 durable edit 与 live side effect/catalog rebuild。

在审计时，`ConfigService` 的锁只串行化配置 edit/save，不覆盖 save 返回后的 runtime apply；Tools admin runtime mutations 也没有拿 `config_apply_gate`，因此它们可能与 Settings apply 并发并对 MCP、Skills、Logging 或 ToolSettings 产生跨入口交错。产品随后确认应迁移到共同 app-level apply gate，同时保留各自 operation-specific mini-transaction；现已按该决策接线。此结论不是增加通用 failure report 解决的问题。

在审计时，产品还需决定 durable config 已更新但 live apply 失败时的用户语义、是否提供显式 retry/restart、不可逆副作用补偿边界及 Settings 错误呈现。后续已确认并实现部分 apply failure、重启从磁盘恢复、无自动 retry/compensation 等语义；不增加 config rollback、显式 retry、跨 subsystem compensation。

## 实现跟进（2026-09-28）：Settings phase apply 失败观测

逐项审计 Settings phase 后，`SettingsApplyOutcome` 增加 typed failure kind；`SettingsApplyObservation` 和结构化日志记录 `runtime_owner`、`tool_catalog_rebuild` 或 `router_prepare`。Settings 对同一 durable snapshot 仍不自动 retry、不 compensation、不 rollback，首个 fatal phase 仍停止后续执行。

| Phase | 可传播失败与记录方式 | 当前无失败返回的操作 |
|---|---|---|
| Router prepare | 原有 builder error 经专用标记记录 `router_prepare` | — |
| Input pipeline | Agent media strategy 的 poisoned mutex 错误记录 `runtime_owner` | Pipeline config 替换无失败分支 |
| Shell | Tool catalog rebuild 拒绝记录 `tool_catalog_rebuild` | Platform snapshot 替换无失败分支 |
| Security | 不产生 coordinator fatal | 授权/MCP policy 内存替换无失败分支 |
| MCP config / monitors | 连接及 monitor 状态仍由 MCP manager status/warning 记录 | 配置接收和 monitor 启动接口无 fatal result |
| Router publish | Agent router mutex 错误记录 `runtime_owner`；catalog 拒绝记录 `tool_catalog_rebuild` 且标记 Router 已发布 | Agent Router 替换后发生 catalog error 时仍发送 `llm:config_changed` |
| Context limits | Tools catalog 拒绝及 Agent poisoned mutex 错误分别记录；pipeline limit 替换无失败分支 | — |
| Session runtime | Agent max-step mutex 错误记录 `runtime_owner` | executor 并发上限调整无失败分支 |
| Tool settings | Settings 现在经 `ToolsManager::set_tool_settings` 更新 platform、授权镜像和 catalog；catalog 拒绝记录 `tool_catalog_rebuild` | 授权镜像替换本身无失败分支 |
| Skills / logging / hotkey | 延续原有可传播错误和 warning-only event 语义 | — |

Tools catalog rebuild 返回 typed `CatalogRebuildOutcome` / `CatalogRebuildError`；Settings setters 不再吞掉 registry 对重复工具名的原子 rebuild 拒绝。部分 owner 已先更新 platform 或授权镜像时不做回滚，coordinator 按既定策略返回部分 apply 失败并保留 durable 配置。配置文件格式、数据库、Tauri IPC 和 phase 顺序不变；内部 Rust setter 改为 typed `Result`。新增回归覆盖 typed outcome、锁 poisoning 诊断、fatal 停止后续 phase，以及 Router 已发布时的准确观察状态。

## 审计结论与验证（2026-09-25）

本切片只增加审计回归测试并记录产品决策边界。测试固定：每个可传播 fatal 的 Settings phase 失败后后续 phase 不再调用；warning-only hotkey event 仍由单独测试验证；Settings 和 model 在 runtime apply 失败后保留已保存配置；相同 payload/mutation 再执行为 no-op，不会隐式重试 runtime apply。现有 phase 顺序、prepare→publish 与错误脱敏测试继续作为行为基线。

无需数据重置。完整逆操作、可重试 phase 和其他 config writer 的 gate/convergence 不在本切片范围。
