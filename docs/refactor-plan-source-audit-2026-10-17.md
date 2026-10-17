# Haven 新一轮架构重构方案（源码审计）

> 日期：2026-10-17
> 状态：Slice 1–6 已完成（Slice 4: ADR 1303–1306；Slice 5: ADR 1307–1311；Slice 6: ADR 1312）；Slice 7 中 PDF/OpenXML parser 拆分已完成（ADR 1313），当前复核 command DTO owner；任何时刻只推进一个 Active 切片。
> 范围：Rust workspace、Tauri IPC/event bridge、Svelte UI 与相关架构/契约文档。

## 1. 目标与审计方法

本轮按实际职责、生产调用链、可变状态读写方、跨 crate 消费者和近期代码改动复核现有架构。目标是让每个运行时状态、策略和 wire contract 有明确 owner；把真实独立职责放进私有模块；合并已经证明会漂移的重复实现。允许删除旧内部接口，不保留无消费者的兼容层。

先读当前实现，再把既有架构文档、路线图和 ADR 当作待验证的假设。若代码与文档不一致，以本次可定位的源码和调用路径为审计依据，并修正文档。安全、持久化或恢复风险会提高失败场景覆盖、分步迁移和门禁强度，但不作为移出本轮队列的理由。行数只用于定位职责，不单独决定拆分。

本轮使用 Rust crate 依赖、公开 facade/port、命令注册及 handler、生产调用点、UI 状态读写、存储事务和近期历史交叉核验；三组并行只读审查分别覆盖 Rust 执行/存储、安全与工具链、App/IPC、UI 状态。所有建议在汇总前回到当前源码复核。此次只做审计和方案文档，没有修改运行时代码，也没有运行测试。

审计时工作树另有改动：`crates/app-binary/gen/schemas/acl-manifests.json`、`desktop-schema.json`、`windows-schema.json`、`crates/tools/src/document.rs` 和 `docs/adr/README.md`。这些不属于本方案文档编辑范围；后续实施必须保留并分别审阅，尤其 `document.rs` 的解析器整理不能覆盖已有用户修改。

## 2. 当前职责地图

以下是由当前依赖方向与生产调用确认的 owner，不是按旧架构图照搬：

| 层 / crate | 当前应持有的职责 | 本轮结论 |
|---|---|---|
| `haven-common` | 跨 crate 稳定类型、纯函数与配置模型；`ConfigService` 保持其已定义的有限配置快照/patch owner | 不搬入业务流程；当前无证据支持拆 crate |
| `haven-platform` | Windows/OS、凭据、文件、进程等平台边界 | 继续做窄平台适配；无理由将调用方业务策略下沉到此处 |
| `haven-llm` | provider 协议、请求/响应、重试、流式与媒体适配 | Router 与 provider 层保持一个 LLM owner；没有独立 crate 消费边界可支持拆分 |
| `haven-memory` | SQLite schema、durable event、事务投影、恢复和记忆持久化 | `SessionStore` 是唯一持久化 owner；文件内职责需要拆开，事务不能拆散 |
| `haven-agent` | SessionActor/ReAct/session 生命周期、会话事件组装和 session ToolRun 授权路由 | 保持 actor/state owner；`tool_runner.rs` 需按步骤持久化、执行、确认子职责拆私有模块 |
| `haven-tools` | 工具目录、执行、能力适配、授权策略与执行前安全复核 | `AuthorizationEngine` 继续是唯一策略 owner；安全策略文件内部应拆开 |
| `haven-app-binary` | 唯一组合根、Tauri command/event DTO 与 adapter、桌面生命周期 | 保持 AppState/ApplicationRuntime；AppCommand UI 确认 owner 与 admin command adapter 需从杂糅 handler 文件抽出 |
| Svelte UI | 页面加载编排、交互状态、展示及 Rust 边界校验 | 页面局部草稿归页面；shell 级通知运行配置应只有一个运行时投影 owner |

没有发现应合并或拆分 workspace crate 的依赖图证据。当前主要收益来自同 crate 私有模块切分、明确状态 owner、减少人工同步副本，而不是增加 crate、服务容器或公共 facade。

## 3. 已确认的高价值问题

### 3.1 通知设置存在同进程 stale bug

`ui/src/routes/+layout.svelte` 持有 `notifyCfg`，session 和 permission 通知 handler 从它读取 in-app 偏好；同一文件中的 ToolRun completion gate 改读 `ui/src/lib/toolRunCompletionNotificationSettings.ts`。`ui/src/lib/views/SettingsView.svelte` 保存全量 notification 后只更新 ToolRun store，没有同步 layout 的 `notifyCfg`。因此保存后同一进程内不同类别通知会读取不同版本。

Settings 保存还有一个必须覆盖的分支：`SettingsView.svelte` 将 partial-apply 错误识别为“磁盘已写入、部分运行时应用失败”，但该路径也未刷新通知投影。notification 偏好本身没有独立 runtime apply 阶段；已持久化的 partial-apply 仍应立即更新 shell 的通知投影。普通持久化失败则不得更新。layout 初始异步加载也需避免晚到的旧 snapshot 覆盖保存后的值。

**目标：** 长期存活的 `+layout.svelte` 成为通知运行时配置唯一 owner，包含 `tool_run_completed`。SettingsView 继续持有自己的全量表单草稿，通过窄回调在成功保存和 durable-first partial-apply 后更新 shell；普通失败不更新。删除 `toolRunCompletionNotificationSettings.ts`，ToolRun gate 读取同一份 `notifyCfg`。不新增 IPC/event 或全局 store。

### 3.2 命令边界 metadata 已经发生静默漂移

命令目前登记在 `crates/app-binary/src/commands/contracts.rs`，实际 handler 数为 91。相同 boundary/security 摘要也手工写在 `ui/src/lib/contracts/commands.ts` 和 `docs/ipc-contracts.md`；`scripts/check-ipc-contracts.ps1` 只比对命令名称集合，不比较 metadata 值。已有差异：Rust 对 `execute_skill` 记为经 `AuthorizationPort` 授权，handler 也实际使用该 port；TS 和 IPC 文档仍写 `AuthorizationEngine`。Rust 对 `resolve_confirmation` 记载 retryable error 会保留 pending，而 TS 摘要没有该不变量。

成功响应 shape 已由现有 Rust IPC generator 从 handler/Serde 声明生成并由 `tauri.ts` 做运行时结构校验。事件 payload shape 也按已登记 channel 生成，并由五个 UI event mapper 在领域语义处理前校验；剩余缺口是 App emitter 的 AgentEvent→channel 与 AgentEvent→DTO 两个 match 可选出互不匹配的一对，且 generator 的 channel→DTO shape 表没有与 emitter 编译期绑定。

**目标：** Rust `COMMAND_CONTRACTS` 保持 boundary/security 的权威登记；TS 和需要的文档数据由它生成，或至少由门禁逐值比较，不能再只校验名称。事件映射一次生成 `{ channel, payload }` 投影，并增加检查确保它与生成 shape registry 的 channel/DTO 配对一致。保留动态 JSON 的既有开放边界，不把它误当作缺失校验。

### 3.3 AppCommand 确认状态机分散在 App 三个文件

`UiConfirmationPending` 与 `ui_confirmations` map/init 在 `app_state.rs`；入队、事件发布和超时在 `commands/mod.rs`；统一 `resolve_confirmation` 及 AppCommand 仲裁、授权接受、永久权限写入和 continuation 在 `commands/session.rs`。该 map 是可变状态 owner 的实现，却能由多个 handler 直接锁定。跨 owner 的 `resolve_confirmation` 同时服务 Session、ScheduledToolRun 和 AppCommand，因此它本身应保留为薄的 App router。

`commands/mod.rs` 还放了 admin 请求授权、风险确认、执行与 UI 收尾。它们是 App/Tauri continuation adapter，不是 Tools 领域策略；policy 必须继续通过 Tools 的 `AuthorizationPort`。

**目标：** 新的 App interactions/confirmation 私有 owner 持有 AppCommand pending request 的 insert/take/expiry/arbitration/continuation；从跨 owner resolver 中分离 AppCommand 实现，但保留薄路由。admin authorization 和 UI continuation 归专门的 command adapter。不要复制授权策略、改变 pending retry/expiry 语义或创建第二份请求表。

### 3.4 SessionStore 文件同时承载多组持久化操作

`crates/memory/src/repositories/session_events.rs` 中生产代码包括 session tool-step、resume projection、fork、ingress seed、pending inputs、recovery partial/terminal ToolRun、event append/page/replay、transcript commit/projection、rollback、usage、branch/recovery marker 与 projection truncation。它已包含部分私有子模块，但主文件仍同时实现这些互异的操作族；约 3.6K 行生产实现和约 3K 行 inline tests 不是单纯行数问题。

必须保持的同一权威与原子边界：`SessionStore` facade 不变；event append 与 transcript projection 在同一 SQLite transaction；rollback 的 event cursor 与 projection cutoff 继续分别表达，rollback 与 usage compensation 在同一 transaction；广播与 cache invalidation 仍只在成功 commit 后发生。`Database::write_session_projections` 和 projection intent 留在 Memory 内部；Agent 不取得 backing database 或投影 DTO。

**目标：** 将 fork、ingress、pending-input、recovery、tool-step 等非 event-core API 先迁入私有子模块；再按 event/model、append/commit、replay/read、rollback/usage 拆分事务实现和测试。多个文件通过同一个 `SessionStore`/connection 和私有 helper 协作，不创建第二个 store 或并行恢复来源。这个高影响切片按纵向事务边界拆步骤并直接实施，不以风险作为暂缓条件。

### 3.5 Tools 安全策略与 Agent ToolRunner 的文件职责过宽

`crates/tools/src/security.rs` 同时包含授权 DTO/port、唯一 `AuthorizationEngine`、receipt/proof、执行期 side-effect permit、MCP/Skills process-start authorization、文件读写 policy、路径/重解析检查、本地安全矩阵和 prompt summary。它们有不同调用者与生命周期，但应继续由一个 `AuthorizationEngine`、一份 `SafetyConfig` 和 `AuthorizationPort` 统一策略。`authorize_for_execution`、`verify_receipt`、`verify_execution_proof` 共享边界检查，当前重复 disabled operation、host process、network、path 与 deny 判定；抽共同的有序纯检查，保留各自不同的 grant、receipt、proof 与 critical-floor 规则。

`crates/agent/src/session/tool_runner.rs` 则同时负责 session step identity/persistence、不同工具执行模式与授权门、scheduled confirmation/claim/terminal lifecycle。Agent 仍是会话与 scheduled 生命周期 owner；Tools 仍是策略 owner。

**目标：** `security.rs` 内拆为 contract/engine/execution/filesystem/prompt 等私有模块，保留稳定 facade 和唯一 engine。先移动现有实现和测试，再统一重复的纯安全边界检查。`tool_runner.rs` 内拆为 step persistence、execution、confirmation/lifecycle 子模块，继续共用当前 `SessionSupervisor`，不添加第二 state owner。

## 4. 文件级分流

| 文件 / 文件组 | 决定 | 目标职责 / 备注 |
|---|---|---|
| `ui/src/routes/+layout.svelte`、`ui/src/lib/views/SettingsView.svelte`、`ui/src/lib/toolRunCompletionNotificationSettings.ts` | **合并运行时通知配置 owner** | layout 持有完整通知运行投影；SettingsView 通过回调同步持久化结果；删除仅由这两个调用者使用的 ToolRun settings store，保留 `toolRunCompletionNotificationProjection.ts` gate/呈现逻辑 |
| `crates/app-binary/src/commands/contracts.rs`、`ui/src/lib/contracts/commands.ts`、`docs/ipc-contracts.md`、`scripts/check-ipc-contracts.ps1`、`crates/app-binary/src/bin/generate_ipc_contracts.rs` | **统一 metadata 权威** | Rust registry 为源；生成 TS/文档副本或门禁逐字段对比；修复 `execute_skill`、`resolve_confirmation` 已知漂移；不手工拆 `generatedCommands.ts` |
| `crates/app-binary/src/event_bridge.rs`、`crates/app-binary/src/events.rs`、事件生成器与 `scripts/check-ipc-events.ps1` | **合并 AgentEvent 投影登记** | 一次匹配返回 channel+DTO；核对 shape registry 配对；保留 ToolRun 已使用的成对 projection 模式、chunk seq、title 注入与 lifecycle 共道行为 |
| `crates/app-binary/src/app_state.rs`、`commands/mod.rs`、`commands/session.rs` 与新私有 command 模块 | **迁移确认/admin 职责** | AppCommand 确认 owner 及 admin adapter 独立；AppState 仍是唯一组合根；`resolve_confirmation` 留作多 owner router |
| `crates/memory/src/repositories/session_events.rs` 及现有 `session_history.rs`、`session_records.rs`、`session_authorization.rs` | **拆私有存储模块** | fork、ingress、pending input、recovery、tool steps 与 event core 分开；保留 SessionStore 单一 facade、transaction、sequence、projection、rollback 与 post-commit 发布边界 |
| `crates/tools/src/security.rs` 与 `crates/tools/src/lib.rs` re-export | **拆私有安全模块** | 一个 engine/配置锁；contract、execution proof、filesystem capability、prompt summary 分域；无外部消费者的安全矩阵可降 visibility，`is_safe_local_path` 仍保留实际跨 crate API |
| `crates/agent/src/session/tool_runner.rs` | **拆私有执行模块** | step persistence、tool mode execution、confirmation/scheduled lifecycle；SessionSupervisor 继续独占协调状态 |
| `crates/tools/src/document.rs` | **低优先私有 parser 拆分** | PDF 与 OpenXML(PPTX/DOCX/XLSX) parser 可放入子模块，主 façade 继续拥有格式选择、容量/取消、页/文本 budget；合并时保留现有工作树改动 |
| `crates/app-binary/src/commands/skills.rs`、`commands/external.rs` | **低优先窄 helper** | 可共用最底层文件管理器进程启动；`open`/`reveal` 目标、调用授权和错误语义仍由各命令保留 |
| `crates/app-binary/src/app_state.rs`、`bootstrap.rs`、`runtime.rs` | **暂留组合根；条件式源文件拆分** | AppState 装配、启动恢复、清理有可分私有职责，但不得新增 Runtime/service container。完成高优先级 owner 后再拆 bootstrap/cleanup 文件 |
| `crates/app-binary/src/commands/contracts.rs` 中多域 DTO | **低优先按 command domain 归位** | metadata registry 与多域 DTO 仓库职责不同；先处理 metadata 权威，不因搬 DTO 改变 IPC DTO owner |
| `crates/agent/src/session/mod.rs`、`crates/agent/src/memory_worker.rs`、`crates/llm/src/router.rs`、`crates/tools/src/builtin/admin.rs`、`ui/src/lib/views/SettingsView.svelte` | **本轮保留** | 当前生产 owner 连贯，体量大部分来自 inline tests 或完整表单/流程；没有已证明的双状态源足以支撑拆分。后续按函数职责审查，不按行数自动拆 |
| `ui/src/lib/contracts/generatedCommands.ts` | **保留生成文件** | 由 Rust IPC generator 生成；只能改 generator/源 DTO，不手工重排产物 |
| `ui/src/lib/components/ToolResultCard.svelte`、`MemoryView.svelte`、ToolRegistry/DeferredToolCatalog/SessionToolOverlay、各 workspace crate 边界 | **保留现 owner/独立概念** | Ask 卡拆分、History 视图重整或 Catalog 合并目前缺少生产状态漂移、重复 owner 或依赖收益证据 |

## 5. 实施顺序与分片验收

以下顺序按真实缺陷、状态权威和依赖关系安排。风险只决定细分和验收强度；不设置“高风险暂缓”队列。每完成一个切片，更新路线图后再进入下一项。

### Slice 1 — 修正通知设置同进程生效

修改 layout、SettingsView 及相关投影测试；移除旧 store/test。覆盖成功保存、普通持久化失败、durable-first partial-apply 和初始 load 晚到四种顺序；确保所有 session/permission/ToolRun 通知读取同一配置。UI 变更运行 UI check、UI tests；不改 Windows 通知 backend 的 ConfigService 读取路径。

### Slice 2 — 消除命令 metadata 与事件投影漂移

先确定 generator 以 Rust registry 产出 TS 机器副本，并将文档表格改为生成/受门禁约束的摘要。metadata 值逐项校验，覆盖现有 `execute_skill`/`resolve_confirmation` 差异。随后把 AgentEvent channel+payload 合成一次投影，并将 emitter projection 与生成 shape registry 绑定校验。保留动态载荷、chunk 顺序、错误清洗与通知副作用。

验收包括 IPC command/event generator drift checks、Rust workspace check/clippy/tests、UI check/tests/build；新增事件必须单点登记并由每个 mapper 做生成 shape 校验。

### Slice 3 — 收拢 AppCommand confirmation 与 admin adapter

一次迁一条 confirmation 完整调用链：queue → requested event → timeout → resolve router → owner arbitration → grant/apply → continuation。由一个 App owner 管理 pending 状态和生命周期，保留不同 owner 路由、request ID、pending retry、过期、session grant 写入和事件顺序。将 admin request/authorization/finalization 放入单独 App command adapter，policy 仍从 Tools 读取。

验收覆盖 Session/ScheduledToolRun/AppCommand 三路 resolve、过期与并发 resolve、retryable failure pending 保留、拒绝与 granted scope、非敏感 renderer DTO、UI 交互和后端清理。另补 Session grant 已持久化而 `interaction_resolved` append 失败后的生命周期链路：当前测试覆盖同 actor 重试；启动时 Paused session 不会自动恢复，只有显式 `reopen_session` 才会重建 durable grant/request 并重新安排 expiry，未加载 actor 的 resolve 应返回 stale。验证该行为与 UI 打开流程一致，不承诺后台 timer 跨进程存活。跨 App/Agent/Tools 改动使用 workspace 与 UI IPC 门禁。

### Slice 4 — 拆分 Memory SessionStore 内部实现

四个子切片顺序执行：

1. **已完成（ADR 1303）：** fork、ingress、pending input、recovery partial/terminal result、tool-step API 移入私有模块；既有 facade 测试覆盖身份、幂等、失败和恢复行为。
2. **已完成（ADR 1304）：** 将 event model/append 与 transcript commit/projection 放入私有模块；append 与物化仍同事务，失败回滚、post-commit 发布和共享 append helper 不变。
3. **已完成（ADR 1305）：** 将 replay/page/active transcript read 收入私有模块，保留 sequence、branch/compaction boundary 和一致读取语义。
4. **已完成（ADR 1306）：** 将 rollback、usage、branch/recovery marker 与 Memory event cursor 收入私有模块；event cursor、projection cutoff 与 usage compensation 的原子事务边界保持不变。

所有方法继续由单个 `SessionStore` 实现/协调，projector 保持 Memory 私有。每子切片都迁移对应故障/恢复断言；最终需覆盖 projection failure 回滚 append、COMMIT 后 publish、stale rollback cursor 拒绝、rollback+usage compensation 同事务、容量限制和 active-root compaction。因涉及持久化恢复，执行前后跑 workspace locked tests、严格 Clippy 与适用 UI/IPC 构建检查；未触及持久 schema 时不制造数据库重置步骤。

### Slice 5 — 拆分 Tools security owner 内部职责

**已完成（ADR 1307–1311）。**

1. **已完成（ADR 1307）：** permission prompt summary 与本地安全矩阵进入私有 `security/prompt.rs`，其七项摘要测试随模块迁移；`is_safe_local_path` 进入私有 `security/filesystem.rs` 并继续作为 App 使用的公开入口。矩阵没有 `haven-tools` crate 外的消费者，已从公共 re-export 收窄。授权 engine 与安全检查行为未改动。
2. **已完成（ADR 1308）：** receipt、decision、request、proof/check 类型与 `AuthorizationPort` 进入私有 `security/contract.rs`；`security.rs` 保持稳定 facade，原 crate 公共类型与方法路径不变。proof/revision/deny 行为测试仍与唯一 engine 的 policy fixture 集中验证，没有可独立迁移的纯 DTO 测试。
3. **已完成（ADR 1309）：** proof-derived side-effect authorization、live policy permit、MCP stdio process-start permit 与 Skill process-start adapter 进入私有 `security/execution.rs`。执行期仍从唯一 engine 读取同一配置锁并复核 generation/revision；全局、tool 和 session revoke 取消路径不变；MCP stdio 与 Skill proof permit 继续使用不同的 gate。
4. **已完成（ADR 1310）：** FileRead/FileWrite policy、路径解析/允许判断 helper 与七项独立路径/句柄安全测试进入私有 `security/filesystem.rs`。原授权 roots 仍由唯一 engine 派生；`security.rs` 保持既有 crate 内 facade re-export 和公开 `is_safe_local_path` 入口。
5. **已完成（ADR 1311）：** `AuthorizationEngine`/`SafetyConfig`、状态/revoke 实现与授权测试进入私有 `security/engine.rs`；`security.rs` 继续 re-export 原 `AuthorizationEngine`。`authorize_for_execution`、`verify_receipt` 与 `verify_execution_proof` 共用按“禁用操作 → host-process sandbox → network → path/sandbox”顺序执行的硬边界检查；各入口独有的 grant、critical floor、receipt/proof 绑定和时效规则保持各自验证。完整 Windows workspace tests 以单线程 harness 通过，strict Clippy 与格式检查通过；并发子进程用例波动已在 ADR 中记录。

整个 Slice 保留唯一 AuthorizationEngine/SafetyConfig/AuthorizationPort；保留 critical floor、grant scope、receipt/proof 绑定与时效、revoke 生命周期、MCP stdio 与 Skill process-start 两套不同 permit，以及网络/路径/reparse 和执行期 live policy guard。最终验收覆盖 deny/revision 后重验、tool/session 局部 revoke、MCP/Skill 子进程入口，使用 workspace locked tests/clippy；Windows release/HCS quota 验收记录在 §5.1 独立 Open Gate。

### Slice 6 — 拆分 Agent tool runner

**已完成（ADR 1312）。**

step identity/persistence、foreground/background/Code Mode/workflow execution routing、session/scheduled confirmation 与 ToolRun terminal lifecycle 已分别迁入私有子模块，继续使用唯一 `SessionSupervisor`、现有步骤 ID 和 confirmation 状态。Agent 继续拥有 session/scheduled 生命周期，Tools 继续拥有授权策略。Agent crate 测试与 strict Clippy、workspace 单线程测试与 strict Clippy、格式和 ADR index 检查均通过。

### Slice 7 — 收拾剩余窄职责文件

**当前 Active：** 复核 `commands/contracts.rs` 的 domain DTO 归属；PDF/OpenXML parser 已按 ADR 1313 拆入私有模块。

按生产调用点复核剩余候选，不因路线图列出候选而自动迁移实现。文件工具没有直接启动外部进程的生产调用点，因此不创建共享 spawn helper；AppState constructor、runtime task 注册和 deferred bootstrap 继续结合其初始化顺序评估是否需要拆分。DTO 归位只在 handler 模块是唯一实际消费者且能保持原 IPC 投影时执行。

按生产调用点验证并拆 `document.rs` 的 PDF/OpenXML parser；只共享文件管理器最低层 spawn helper；视初始化顺序和测试可见性决定是否把 AppState bootstrap/cleanup 移到私有文件。以上均不改 crate 边界，不新增可变 service。合同 DTO 归位也放在此 slice 的小步执行。

## 6. 保留边界与停止条件

- 保持跨 crate 依赖单向；不新增 workspace crate，除非新 owner 有独立稳定消费者和明确依赖收益。
- 不暴露 `SessionStore` backing DB、projection intent、AuthorizationEngine 实例或可变 registry 给消费者。
- 不复制 authorization policy、session state、notification preference 或 IPC metadata 权威；生成文件是派生输出。
- 不为减少行数搬移测试或引入泛化 framework；模块按生产职责和失败归属切分。
- 私有模块整理不改变 IPC wire、数据库 schema、durable event、ID 或用户可见安全语义；若实现中确需改变这些契约，另行记录兼容/重置策略与 ADR。项目不要求内部兼容层。
- 实施门禁按 `docs/development-standards.md` 的实际变更面选择；门禁负责证明正确性，不作为不推进的理由。未通过时修复切片，不把任务退回高风险 Candidate。

## 7. 已发现的过时审计结论

| 文档 / 结论 | 当前源码事实 | 处理 |
|---|---|---|
| `architecture-refactor-roadmap.md` 声称暂无结构候选，并将 `session_events.rs` 标为“高风险 Candidate，暂缓” | source audit 已定位多个独立的生产 API/操作族和内部职责；原子性边界可以在同一 SessionStore 内保留 | 本方案重列为执行切片；风险转为事务分片与验收条件 |
| `architecture-refactor-roadmap.md` 将 grant append failure 后 actor 退出的 stale request recovery 标为未定义 | 当前 gate 串行化 resolve/end；actor append 成功前保留 pending；同 actor 重试已有测试。重启只自动恢复 `Pending` 状态 session；`Paused` 需显式 reopen 才 replay grant/request 并重挂 expiry | 明确当前 lazy-recovery 行为；Slice 3 覆盖 grant-write/resolved-event-fail/restart/reopen/expiry 与未加载 actor 返回 stale，不添加跨进程内存 timer 假设 |
| `architecture-output-contract-inventory.md` 称 79 个命令响应、生成器未逐个产生响应 shape | 当前 registry 是 91 个 handler；Rust generator 产生递归 response shape；原表还漏列了 12 个新增命令，现逐一补齐 | 修正 count 与生成覆盖描述，并将 12 个 command response 分类补入；机器 metadata 的真实缺口是 boundary/security 值未逐字段比对 |
| `architecture-output-contract-inventory.md` 称事件字段级自动 checker 尚未覆盖 | 当前生成器为 36 channels 生成 payload shape，事件脚本比较 shape/channel 集，5 个 UI mapper 使用生成结构 validator | 改为描述当前 coverage，并指出 AgentEvent 到 channel/DTO 配对还未与 shape 登记绑定 |
| `docs/ipc-contracts.md`、`commands.ts`、Rust metadata 的安全摘要被当作一致 | `execute_skill` 已有 AuthorizationPort / AuthorizationEngine 文案不一致，checker 只比命令名 | 纳入 Slice 2，修复已存在 drift 并让未来值变化失败 |
