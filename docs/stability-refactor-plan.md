# Haven 稳定性与可维护性重构计划

> 状态：进行中  
> 制定日期：2026-08-26  
> 版本定位：个人单机助手的测试版本；允许破坏性重构，不承诺旧数据库、旧配置或旧快照兼容。

## 目标与非目标

本轮目标是让 Haven 成为可稳定迭代的个人 Windows 助手：工程质量门禁可重复、核心边界可验证、失败可诊断、危险操作可控。

本轮不以新增功能、旧数据迁移或多用户/云端协作为目标。遇到阻碍正确设计的兼容代码时，优先删除并提供明确的重置说明。

所有开发与重构必须遵守 `docs/development-standards.md`；规范、清晰和可验证性优先于短期功能速度。

具体执行顺序、阶段退出条件和重构模板见 `docs/refactor-execution-guide.md`。

## 不变量

- 会话事件是 ReAct 恢复的权威数据；投影表只能由统一投影路径写入。
- 模型供应商实现只放在 `haven-llm`；宿主层只负责 Tauri 装配与 IPC。
- 所有本机写入、进程执行、系统控制必须经过安全网关；确认策略不能被 UI 绕过。
- 每项跨 crate、数据库 schema、IPC 事件或安全语义改动必须有回归测试与简短 ADR。

## 分期

### P0：恢复工程基线（已完成：2026-08-26）

- 日志初始化改为可恢复失败，禁止目录冲突导致启动或测试 panic；测试不得读写真实用户数据目录。
- 修复 `cargo clippy -- -D warnings`，固定 Rust 工具链版本；CI 同时覆盖 Windows 与 Linux。
- 恢复文档链接完整性，新增面向开发者的入口、运行、测试、重置和发布说明。
- 将现有分散规范收敛为可执行的开发治理标准，并把 CI 门禁、ADR、DoD 和安全检查纳入日常开发。
- 将覆盖率改为趋势报告；若启用阈值则作为真正门禁，不能静默忽略上传或生成失败。

#### 已完成记录

- 2026-08-27：固定 Rust 1.98.0、Node 24.20.0 与 pnpm 11.24.0 的质量门禁，Linux/Windows 均执行 Rust workspace 测试与 UI 门禁；补齐 README、发布重置说明与 ADR 目录（ADR 0001）。
- 2026-08-26：Agent 的严格 Clippy 清理完成；Shell 测试改为临时目录与 PowerShell 内置字节输出，避免写入默认工作目录或依赖 PATH 中的 Python。
- 2026-08-26：MCP 配置生命周期与客户端集成测试改为进程内 HTTP MCP 端点，并删除 Python fixture；窗口单元测试不再要求可交互桌面。OCR 未配置视觉路由时不采集屏幕（ADR 0002）。

### P1：删除过时设计，固化边界

- 清理仅为旧配置、旧快照、旧 provider 名称保留的兼容分支；在发布说明中明确“升级需重置”的范围。
- 为 Tauri 命令、事件载荷和持久化 schema 建立显式 DTO/版本边界，减少无类型 `serde_json::Value` 的跨层传播。
- 用自动检查保护 crate 依赖方向，并以 ADR 记录数据库、权限与会话语义的重大决定。
- 为本机工具建立安全回归矩阵：路径规范化与重解析点、授权继承、取消/超时、外链以及子进程生命周期。

#### 已完成记录

- 2026-08-26：会话域第一条 IPC 调用链完成 DTO 化：生命周期、错误、标题更新与删除事件统一由 app 的命名 DTO 发出；前端在唯一监听边界转换为 camelCase，并登记命令、事件顺序、幂等和敏感字段限制（ADR 0003）。
- 2026-08-26：删除安全确认模式 `always` → `ask` 的静默配置兼容；旧配置改为备份并以安全默认值启动，发布重置说明已标注（ADR 0004）。
- 2026-08-26：Tauri WebView 由 `csp = null` 改为生产与开发分离的明确 CSP；生产默认拒绝脚本执行、任意网络、frame 与对象，仅保留 Tauri IPC、本地资产和已验证的媒体来源（ADR 0005）。
- 2026-08-26：任务域 IPC 完成 DTO 化：后台任务和定时任务通过 app shell 投影为统一 `ActionEvent`，命令与四个生命周期事件不再向前端暴露工具内部 JSON、动态参数、续接提示或本地日志路径；前端在唯一边界转换为 camelCase（ADR 0006）。
- 2026-08-26：设置诊断命令完成命名响应 DTO 化：日志信息、日志尾部和 shell 探测均由前端唯一 contracts 边界校验，畸形响应不再静默进入页面状态（ADR 0007）。
- 2026-08-26：设置诊断命令补齐模型凭据状态 DTO：固定状态字段与 provider 名称扩展点分离，响应不携带凭据（ADR 0007）。
- 2026-08-26：录音与转写命令/事件完成 DTO 化并登记顺序和敏感字段；删除旧采集配置、工具设置与定时任务工具名的静默迁移，旧数据改为备份/重置；CI 自动校验 crate 依赖方向（ADR 0008）。
- 2026-08-26：其余 67 个 Tauri 命令完成统一目录登记；稳定命令响应移除无必要的 JSON 外壳；SafetyGateway 补齐重解析点、源/目标路径和授权继承负向矩阵（ADR 0009）。
- 2026-08-26：移除 Phase-7 ReAct 快照重建和未命名媒体 provider 的本地凭据回退；旧快照与媒体配置要求完整重置后重新配置（ADR 0008）。
- 2026-08-27：补齐应用壳层与 Agent 事件 DTO：39 个事件统一由 Rust 名称常量、命名 wire DTO 和前端 contract 登记；新增 `scripts/check-ipc-events.ps1` 保护 channel 集合与 snake_case → camelCase 边界（ADR 0009）。
- 2026-08-27：媒体能力的持久化配置仅保留命名 provider 与能力参数；STT/TTS/文生图凭据改为解析后的运行时 DTO，旧媒体 provider 名或本地凭据会备份配置并以默认值启动，避免再次读入旧兼容字段（ADR 0008）。

### P2：按稳定接口拆解热点（已完成：2026-08-31；平台适配暂缓）

- Memory：将 schema、图谱写入、查询/排序、嵌入与迁移策略分离；仓库层不承担业务推理。
- LLM：提取供应商适配器共享的请求、流式、用量和重试管线；每个 provider 只保留协议映射。
- Agent：将 loop、恢复、投影、队列和副作用 Hook 保持独立；禁止从快捷修复重新穿透层级。
- UI：拆分聊天页的会话状态、事件归并、输入和渲染；将工具结果渲染改为按工具类型注册的组件，避免继续扩大单一页面与卡片组件。

#### 暂缓项（不计入 P2 完成条件）

平台适配（见 ADR 0015）暂缓并移出本阶段，后续单独立项处理音频实时回调与设备恢复、tract CPU 回归、进程组取消、Windows 专属能力降级、通知/自启适配以及 Linux 桌面构建验收。

#### 已完成记录

- 2026-08-27：P2 Agent 首个切片完成：将 ReAct 回合结束流程从 `react/inject.rs` 移至独立 `react/turn_end.rs`，以 `TurnEndInput` 固定最终事件、并发注入、分支点与暂停的调用边界；行为与持久化契约不变（ADR 0010）。
- 2026-08-27：P2 Agent 上下文来源切片完成：以 `react/context.rs` 的 `ContextSource` 聚合队列与 inbox，`react/inject.rs` 仅把拥有所有权的批次经 `apply_transcript` 投影，保留既有顺序、ask gate 清除和低信任净化（ADR 0011）。
- 2026-08-27：P2 Agent 恢复/回滚/Hook 边界收口：恢复候选合并、无快照工具链投影和 MCP 选择移入 `resume_support.rs`；回滚事件操作移入 `rollback_support.rs` 并优先使用 `message_id`；生产 Hook 策略移入 `react/hook_policy.rs`，保持 `events` 为恢复唯一权威（ADR 0012）。
- 2026-08-27：P2 Memory schema 边界收口：当前幂等 schema 与历史迁移目录拆分为 `schema.rs` / `migrations.rs`，保持 `user_version` 逐步戳记、迁移顺序与 X12 恢复权威不变（ADR 0013）。
- 2026-08-29：P2 Memory 图谱写入边界收口：事实插入、节点关联、用户权威、upsert 与删除集中到 `repositories/fact_graph.rs`，`Database` API 与 X12 契约不变（ADR 0019）。
- 2026-08-29：P2 Memory 查询/排序边界收口：事实行映射、列表/批量读取、FTS/LIKE、标签查询、缓存读取与有效置信度排序集中到 `repositories/fact_query.rs`，保持 `Database` API 与持久化语义不变（ADR 0020）。
- 2026-08-29：P2 Agent/Memory 嵌入编排边界收口：embedding provider 调用、有限索引 catch-up、模型切换清理、向量召回与 LSH 重建集中到 Agent 的 `memory_index.rs`，保持 Memory `Database` API 与召回语义不变（ADR 0021）。
- 2026-08-29：P2 Memory 维护持久化边界收口：事实去重、敏感清理、衰减清理、来源规范化与矛盾候选扫描集中到 `repositories/fact_maintenance.rs`，Agent 继续负责维护调度与 LLM 仲裁，保持 `Database` API 与维护语义不变（ADR 0022）。
- 2026-08-29：P2 LLM 请求策略边界收口：普通聊天、工具聊天、embedding 与流式端点尝试共用 `request_pipeline.rs` 的重试预算快照与总超时执行器，保持 router 路由、fallback、流式聚合和 provider wire 契约不变（ADR 0023）。
- 2026-08-30：P2 LLM 适配器传输边界收口：provider 共用 HTTP client、认证头、状态错误、流式 header 超时与健康检查集中到 `adapters/transport.rs`，保持 provider wire 契约不变（ADR 0024）。
- 2026-08-30：P2 LLM 适配器流式 framing 边界收口：SSE/JSON-lines 行读取、EOF flush 与空 stream chunk 基线集中到 `adapters/stream.rs`，保持 provider wire 契约不变（ADR 0025）。
- 2026-08-30：P2 LLM 适配器 embedding 边界收口：OpenAI-compatible embedding 的 URL、请求体、响应排序/校验与 usage 转换集中到 `adapters/embedding.rs`，保持 provider wire 契约不变（ADR 0026）。
- 2026-08-30：P2 LLM 适配器 web search 边界收口：内置搜索 call 规范化、citation 结果和按 id 去重集中到 `adapters/web_search.rs`，保持 Agent/UI 结果契约不变（ADR 0027）。
- 2026-08-30：P2 LLM 适配器 provider feature 边界收口：vendor 检测、thinking/reasoning 映射、echo 判定与长度限制集中到 `adapters/provider_features.rs`，保持 Chat/Responses wire 契约不变（ADR 0028）。
- 2026-08-30：P2 Agent 事实抽取边界收口：事实抽取 DTO、字段 coercion、标签/谓词规范化、prompt 清洗与 JSON array 提取集中到 `fact_extraction.rs`，保持抽取与持久化语义不变（ADR 0029）。
- 2026-08-30：P2 UI 会话消息状态边界收口：会话消息 map、草稿/会话迁移、rollback 截断与流式 sequence 去重集中到 `ui/src/lib/sessionMessages.ts`（ADR 0030）。
- 2026-08-30：P2 UI 会话用量状态边界收口：token usage、LLM 调用明细、恢复/清理与用量格式化集中到 `ui/src/lib/sessionUsage.ts`（ADR 0031）。
- 2026-08-30：P2 UI 流式事件归并边界收口：chunk 排队、按帧归并、sequence 去重、step block 关联与同步 flush 集中到 `ui/src/lib/streamAggregator.ts`（ADR 0032）。
- 2026-08-30：P2 UI 工具结果渲染注册表首片完成：shell、notify、generic/raw body 按 kind 注册到独立 renderer 组件，ToolResultCard 保留公共卡片壳与复杂工具分支（ADR 0033）。
- 2026-08-30：P2 UI 文件工具 renderer 边界收口：`file` 工具的文件操作、目录和读取结果集中到 `ToolFileResult.svelte`，由 renderer registry 按工具名选择（ADR 0034）。
- 2026-08-30：P2 UI system 工具 renderer 边界收口：机器指标、显示器、环境变量筛选/复制与电源状态集中到 `ToolSystemResult.svelte`，由 renderer registry 按工具名选择（ADR 0035）。
- 2026-08-30：P2 UI process 工具 renderer 边界收口：筛选、显示上限、CPU/内存指标和状态表格集中到 `ToolProcessResult.svelte`，由 renderer registry 按工具名选择（ADR 0036）。
- 2026-08-30：P2 UI window/action/schedule renderer 边界收口：三类结果分别集中到对应 renderer 组件，由 registry 按工具名选择（ADR 0037）。
- 2026-08-30：P2 UI http/clipboard/web_search renderer 边界收口：三类结果分别集中到对应 renderer 组件，由 registry 按工具名选择（ADR 0038）。
- 2026-08-30：P2 UI file_search/files renderer 边界收口：搜索结果集中到 `ToolFileSearchResult.svelte`，由 registry 按工具名与结果 shape 选择（ADR 0039）。
- 2026-08-30：P2 UI agent renderer 边界收口：agent 结果集中到 `ToolAgentResult.svelte`，由 registry 按工具名选择（ADR 0040）。
- 2026-08-30：P2 UI 工具结果解析边界收口：JSON 解码、空内容处理与 custom shape 分类集中到 `ui/src/lib/toolResultParsing.ts`，`ToolResultCard` 保留兼容 re-export（ADR 0041）。
- 2026-08-30：P2 UI 聊天工具栏边界收口：会话切换/token 概览与模型/联网搜索菜单集中到独立组件，路由页继续负责状态与回调编排（ADR 0042）。
- 2026-08-30：P2 UI 会话用量展示边界收口：每步用量聚合、缓存命中率与 token tooltip 集中到 `ui/src/lib/sessionUsagePresentation.ts`，路由页保留响应式状态适配（ADR 0043）。
- 2026-08-30：P2 UI Agent 事件 handler 边界收口：thought/reasoning、web search、补充输入、工具 action/output/observation 的事件到消息投影集中到 `ui/src/lib/chatAgentEventHandlers.ts`，路由页保留状态与监听器编排（ADR 0044）。
- 2026-08-30：P2 UI Agent 用量事件边界收口：`agent:usage` 的 token/费用/调用明细投影与 `agent:compaction` 通知集中到 `ui/src/lib/chatUsageEventHandlers.ts`，路由页保留监听器编排（ADR 0045）。
- 2026-08-30：P2 UI 安全确认事件边界收口：`confirm:requested` DTO 到确认队列项的映射集中到 `ui/src/lib/chatConfirmationEventHandlers.ts`，路由页保留队列、对话框生命周期与授权 IPC（ADR 0046）。
- 2026-08-30：P2 UI 会话生命周期事件边界收口：session created/updated/completed/error/title-updated 的状态投影与终态清理集中到 `ui/src/lib/chatSessionEventHandlers.ts`，路由页保留响应式状态回调（ADR 0047）。
- 2026-08-30：P2 UI 聊天消息时间线边界收口：欢迎态、消息列表、后台等待提示与错误继续按钮集中到 `ui/src/lib/ChatMessageTimeline.svelte`，路由页保留滚动容器与业务回调（ADR 0048）。
- 2026-08-30：P2 UI ask 交互边界收口：选项选择、批量回答、忽略、恢复清理与重复提交防护集中到 `ui/src/lib/chatAskInteraction.ts`，路由页保留输入编排（ADR 0049）。
- 2026-08-30：P2 LLM endpoint 健康与熔断边界收口：熔断器、连续失败统计、半开探测、role 索引与健康槽位初始化集中到 `crates/llm/src/endpoint_health.rs`，router 保留并发存储与请求时机（ADR 0051）。
- 2026-08-30：删除已到期的 UI `stores.ts` 消息/用量兼容 re-export，并删除未压缩 ReAct snapshot 读取回退；旧 UI 导入和旧数据库快照按发布说明迁移/重置（ADR 0052）。
- 2026-08-30：P2 LLM 流式执行边界收口：流式上下文估算、idle scaling、规则门禁、chunk 聚合与首 chunk 前重试集中到 `streaming.rs`，router 保留 endpoint 编排（ADR 0053）。
- 2026-08-30：P2 Agent 事实推理边界收口：增量抽取窗口、transcript 构造、来源解析与提案安全门禁集中到 `fact_inference.rs`，inference 保留调度与写入编排（ADR 0054）。
- 2026-08-30：P2 UI 模型同步边界收口：默认模型发现缓存、设置投影、provider 能力归一化与刷新代次集中到 `chatModelSync.ts`，路由页保留状态与菜单编排（ADR 0055）。
- 2026-08-31：P2 Agent ReAct 控制流重组：以 Run/Turn/ToolBatch 明确模型采样、工具批次和生命周期边界；工具结果按 assistant 调用顺序物化，steering 优先于 follow-up（ADR 0056）。
- 2026-08-31：P2 Agent ReAct 运行态收口：引入 `ReActState` 统一 events/canonical/branch points；sanitize 与失败 retry nudge 均限制在 provider 请求态，暂停恢复不产生隐式 transcript 写入（ADR 0057）。
- 2026-08-31：P2 Agent 请求与流式代次收口：以 `RequestContext` 统一 provider 请求投影；inbox envelope 保留独立边界；thought/reasoning 共用有序 chunk 队列，并以 `agent:stream_reset` 隔离 failover/retry 输出；前端只合并相邻 chunk（ADR 0058）。

## 完成标准

- 固定工具链下：格式化、检查、Clippy、后端测试、前端检查、前端测试、前端生产构建全部通过。
- Windows CI 覆盖桌面后端与路径/日志相关测试；不依赖开发者真实配置目录。
- 依赖图、架构文档、ADR 和代码一致；不存在指向已删除文档的链接。
- 每个保留的兼容层都有到期日期；测试版默认不保留无到期日期的兼容代码。

## Git 历史重置（最后执行）

在稳定版本验收并建立可下载的源代码快照后，创建新的无父提交仓库根并强制更新远端默认分支。该动作必须单独审批，先处理分支、标签、现有克隆、CI 密钥和发布产物；它不应与功能或重构提交混在一起。
