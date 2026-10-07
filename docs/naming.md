# Haven 命名规范

> 版本: v1.145 | 日期: 2026-10-08

本文档统一 Haven 项目各层的命名规则（变量名、函数名、文件名、crate 名、缩写大小写、跨层边界）。规范以现有代码中的事实模式为基础，新代码必须遵循；存量代码若与规范冲突，逐步迁移对齐。

## 总则

- **分层语气不同**：Rust 后端与 Svelte 前端各自遵循本语言生态的惯例，二者仅在跨层边界（Tauri 命令 / 事件 / 数据字段）约定转换规则。
- **一眼可辨**：命名应能区分「类型」「值」「常量」「组件」「模块」，见各层细则。
- **边界语义不撞名**：不同 crate/边界中的类型即使位于不同命名空间，也要能从类型名看出领域或 wire 角色；同名但不同状态空间时用领域限定词，不要求合并状态 owner。例如 Memory durable `SessionEvent` 与 Agent process-local `SessionSupervisorEvent` 各自保留 owner。
- **Serde alias 只表示 wire 名称差异**：字段默认 Serde 名已等于 alias 时删除重复 alias；只保留外部协议确实发送的其它字段拼法，例如 Gemini 的 `finishReason`。Haven 自有配置或 payload 的旧名不作为隐式兼容入口（ADR 0662）。
- **同域不同形状标明角色**：同一领域中的完整 runtime state、稀疏 view 输入或 wire projection 即使字段重叠，也使用能标出约束/角色的不同类型名，不用可选字段数量来猜其含义。
- **闭合契约与开放扩展点分开**：Rust/IPC 已声明的闭合枚举在 UI 直接复用 generated 类型，边界拒绝未知成员；不要为了未来值或旧值映射把它扩成任意字符串。只有协议或扩展点本身定义为开放字符串、且消费者必须保留原值时，才把 raw wire value 与内部归一分类分名。Tool manifest 的 source/group/risk 是闭合 generated enum；`presentation.renderer` 是开放扩展 discriminator，未知 renderer 使用通用 JSON renderer（ADR 0669）。
- **多值结果具名**：若多个返回值各有稳定领域含义，使用具名结构体字段，不用位置元组让调用者记住各索引的语义；例如 `CreatedSession` 保存首条持久用户消息 ID，`ModelPricing` 区分 input/output 价格，`FactProvenanceColumns` 对应不同持久列，`StreamedLlmCall` 区分响应和耗时，`ToolRunEventProjection` 区分 channel 和 payload，`ParsedAgentResponse` 区分解析出的 thought 和 tool calls，`CappedText` 命名限长文本与截断状态，`ChatThinkingExtras` 区分 chat wire 的 thinking object 与 reasoning effort 字段，Anthropic request projection 分别区分 wire messages/system prompt 与 `thinking`/`output_config`，`GeminiContentConversion` 区分 provider content 与 system instruction，`ResponsesInputConversion` 区分 Responses input items 与 instructions，Tools process reader 用 `CappedStreamText` 和 `CappedStreamRead` 区分 decoded text 与 retained bytes/read error，`SkillProcessOutput` 则按 stdout/stderr 命名 Skill 进程结果，Agent 的 `CapabilityProjectedRequest` 区分 media-capability 处理后的 request context 与向 UI 发布的 `MediaPlan`，Common 的 `TruncatedOutput` 则把带省略标记的文本与截断状态放在同一结果中。
- **ToolResult 截断元数据与正文标记分层**：`ToolResult.truncated` 是执行结果级元数据；JSON 正文的 `truncated` 是具体工具的模型可见字段。正文标记为 `true` 时必须同步结果标记；工具正文契约包含该字段时，成功结果使用 `ToolResult::from_output` 统一同步，失败构造从已有正文标记推导结果标记。正文契约不包含该字段时可只设置结果标记，不为了对齐而扩展 JSON shape（ADR 0701）。
- **结果结构区分阶段 owner**：adapter 前的解析/归一结果与对外执行 envelope 即使携带相似字段，也分别命名其阶段职责；例如 MCP content extraction 与 `McpCallOutput` 分开承载内容转换结果和最终工具调用状态。
- **TranscriptProjection 按读取与提交阶段分名**：公开 `TranscriptProjection` 是从 durable event log 同次派生出的 canonical messages 与 ReAct rounds；Agent 内部 `CommittedTranscriptProjection` 是 durable commit 后应用到进程内 `ReActState` 的记录包。两者 shape 和生命周期不同，不合并，也不共用泛名（ADR 0602/0671）。
- **全局网络策略与 HTTP 目标约束分名**：Common `NetworkPolicy` 表示应用安全设置的网络访问模式；HTTP 工具内部 `HttpDestinationPolicy` 只承载目标域 allowlist 与测试 loopback 条件。请求参数和方法分别叫 `HttpRequestParams` / `HttpRequestMethod`，URL 校验/解析动作标为 `validate_http_destination` / `resolve_http_destination`，不使用泛名 `Network*`（ADR 0672）。
- **熔断状态标明 owner**：`ToolCircuitState` / `ToolCircuitBreaker` 属于按工具隔离、阈值和冷却可配置的工具执行保护；`EndpointCircuitState` / `EndpointCircuitBreaker` 是模型 endpoint 的唯一准入与失败状态 owner，过滤打开熔断前请求的迟到完成并支持手动重试重置。`LlmRouter.endpoint_circuits` 按配置模型 ID 索引熔断器，不另存一份健康计数（ADR 0673、0690）。
- **Session 运行态与持久历史分名**：`list_runtime_sessions` / `RuntimeSessionListResponse` 只表示当前进程中驻留且未终结的会话；`list_session_history`、搜索、计数和导出命令表示持久会话历史。跨层的历史查询、结果缓存都带 `session_history` 作用域；全量持久删除叫 `delete_all_sessions`，supervisor 内部运行态清理用 `clear_session_runtime_state_locked`，关闭入口用 `clear_session_runtime_state_for_shutdown`（ADR 0679）。
- **Session IPC 包装模块按实体命名**：前端 `sessionCommands.ts` 集中封装 Session 的运行态列表、历史读取、恢复、生命周期与标题命令；单个函数仍按操作阶段使用 `listRuntimeSessions`、`listSessionHistory`、`getSessionForResume`、`deleteSession` 等领域动词，不因模块收纳在同一个文件而抹去状态范围（ADR 0679）。
- **工具清单明确来源**：`list_builtin_tool_manifests` 只返回内置工具的 manifest，不包含 Skill 或 MCP manifest；命令与响应类型分别标出 `builtin_tool_manifests` 和 `BuiltinToolManifestListResponse`。Tools 与 Skills 的 App 命令按 domain 分模块（ADR 0680）。
- **MCP 服务器集合命令标明实体**：`list_mcp_servers` / `listMcpServers` 返回 `McpServerSnapshot[]`，其中同时包含配置、连接状态和该服务器的工具清单；不要因快照中含工具而命名为 `list_mcp_tools` / `listMcpTools`（ADR 0720）。
- **MCP 单服务器动作标明目标实体**：单服务器重连命名为 `reconnect_mcp_server` / `reconnectMcpServer`，与作用于服务器集合的 `refresh_mcp_servers` 区分；不要省略被操作的 server 实体（ADR 0731）。
- **闭合 IPC 选择复用领域 enum**：`recall_memory.kind` 使用 Memory 拥有的 `MemoryEntityKind` / generated `MemoryEntityKindInput`，不再通过开放 `String` 在 handler 内解析。renderer-only 的 `all` 保留为 UI 筛选项并展开为 `fact` 与 `episode` 两次请求，不传成后端 domain 值（ADR 0732）。
- **确认权限值复用生成 enum**：`resolve_confirmation` 的 `effect`、`scope`、`target` 分别使用 Common `PermissionEffect`、`PermissionScope`、`PermissionTarget` 及 generated input types；`ConfirmationDecision` 继续表示含步骤 ID 和批准语义的 UI callback，再由 shell 转成 request（ADR 0733）。
- **联网搜索模式复用闭合 enum**：`set_web_search.mode` 使用 LLM `WebSearchMode` / generated `WebSearchModeInput`，UI 选择只提交 `off` / `auto` / `always`；可选 `null` 仍表示清除模型覆盖，并不等于显式 `off`（ADR 0734）。
- **Session history status filter 使用严格查询 enum**：搜索与导出请求使用 Memory `SessionHistoryStatusFilter` / generated `SessionHistoryStatusFilterInput`；空 UI 选择映射为无过滤。它与生命周期 `SessionStatus` 分开，避免把生命周期未知值的 fail-safe fallback 暴露为 IPC 输入行为（ADR 0735）。
- **ToolRun completion notification 复用 App wire enum**：`notification:show` 的 source/status/discriminator 使用 App `ToolRunKindDto`、`ToolRunCompletionStatusDto`、`AgentNotificationKind`，前端 payload 类型与 runtime validator 引用 Rust 生成的类型和值清单。Agent 保留通知的 source/status runtime owner，App bridge 显式映射到 wire vocabulary（ADR 0736）。
- **Reasoning effort command 与 provider config 分开**：聊天工具栏的 setter 只接受 generated `ReasoningEffortSelectionInput`（`low` / `medium` / `high` / `off`），`null` 清除覆盖；`ModelConfig.reasoning_effort` 仍是 provider-specific 开放字符串，以支持 `max`、`xhigh`、`none`、`disabled` 等 adapter 输入和 provider 映射（ADR 0737）。
- **Fact source 筛选复用持久化闭合集合**：`list_facts.source` 使用 App `FactSourceFilter` / generated `FactSourceFilterInput`，其值域与 SQLite `facts.source` 约束一致（`user` / `inferred`）；省略或 `null` 表示不筛选。事实输出的 `source` 继续按现有 response projection 序列化（ADR 0738）。
- **Shell availability command 复用配置 shell enum**：`check_shell_available.shell` 使用 Common `ShellChoice` / generated `ShellChoiceInput`，与 Settings 的 `default_shell` 共享 `powershell` / `cmd` / `pwsh` 词汇；未知 shell 不作为有效诊断输入（ADR 0739）。
- **消息角色复用 canonical vocabulary**：持久消息、Agent history、resume/live UI 中的 `role` 使用 Common `CanonicalRole`；数据库文本在写入时由 enum 序列化，读取时严格解析，UI 消费 generated type。Provider wire role 继续由各 LLM adapter 映射；模型发现与模型配置命令用 `request_kind: RequestKind` 选择逻辑请求，UI 请求引用生成的 `RequestKindInput`。消息角色、请求路由与模型配置 ID 是不同身份，不复用 `role` 字段（ADR 0726、0728、0729）。
- **模型操作路由参数明确命名**：`discover_models`、`switch_model`、`set_reasoning_effort` 与 `set_web_search` 均以 `request_kind: RequestKind` 标识逻辑请求路由；参数命令仅修改该路由当前分配的模型。选择指定模型配置时使用独立的 `model_id`（ADR 0728、0729）。
- **持久消息类别与 UI 展示类型分开**：`messages.message_type` 使用 Common `TranscriptMessageKind` 表达 SQLite 闭合集合，写库序列化、读库严格解析并生成到 resume IPC；`StreamMessage.type` 是 renderer presentation discriminator，由 resume mapper 从 durable kind 投影，不复用或冒充持久类别（ADR 0727）。
- **IPC 请求类型按命令 owner 命名**：前端 wrapper 的 request alias 必须从它实际调用的 generated command 派生；即使两个命令的字段形状相同，也分别命名，如 `SetSkillEnabledRequest` / `SetToolEnabledRequest`、`ReconnectMcpServerRequest` / `RemoveMcpServerRequest`，不要用某一个命令的类型 alias 覆盖另一个命令（ADR 0681、0731）。
- **Tool renderer 只按展示职责派发**：`ToolPresentation.renderer` 是开放扩展字符串；UI 仅为当前明确登记的 renderer key 选择专用组件，未知 key 使用通用 JSON renderer。MCP 固定使用 `mcp` renderer，server name 只放在 `ToolIdentity.root` / `root_presentation`，不能兼任组件分派键；builtin renderer 使用明确登记的 key，Skill 使用固定 `skills` root。不要把旧的 `haven`、`admin`、`settings` 别名隐式解释成管理面板（ADR 0682、0703）。
- **Admin capability 唯一拥有管理工具名**：`AdminCapability::tool_name` 唯一映射 `haven_*` 工具名；model operation surface 先解析到同一 enum 再取工具名，不在 request helper 或 metadata 中另存重复字符串（ADR 0683）。
- **Admin operation 统一从组合 owner 构造**：配置操作由 `AdminSurfaces` 通过共享 `AdminServices` 构造；不保留无生产调用方的 `ConfigAdminContext`、`ConfigAdminTool` 或单独 builder 作为第二条装配路径（ADR 0684）。
- **认证方案与凭据分阶段命名**：header policy 使用 `AuthHeaderScheme { header_name, prefix }`；将密钥应用到方案后得到 LLM registry 拥有的 `ModelDiscoveryAuthHeader { header_name, value }`，该类型直接跨 App→LLM API 传递；一次 model discovery 的输入由 `ResolvedDiscoveryAuth { api_key, auth_header }` 表达。含实际凭据的类型不自动派生 `Debug`，避免调试格式意外暴露密钥。
- **模型配置引用与供应商身份分名**：`ModelConfig::provider_name` 指向 `ProviderConfig::name`（用户配置的连接名称）；`ProviderConfig::provider` 与 `ModelEndpoint::provider` 表示供应商身份。Serde/TOML/IPC 字段统一为 `provider_name`；设置编辑器内部使用 `providerName`，只在 generated IPC 边界转换命名风格。按连接名查找使用 `LlmConfig::provider_config_by_name`（ADR 0632）。
- **配置投影视图引用生成字段**：UI helper 仅消费设置 DTO 的部分字段时，用 `Pick<GeneratedInput, ...>` 派生投影，不手写同形字段；确有草稿中间态允许 `null` 的字段在投影中显式拓宽，并与 required wire contract 区分。`apiStyle.ts` 的 `ProviderStyleInput` 基于 generated `ProviderConfigInput`，只为 `provider` 与 `base_url` 保留 UI 草稿 nullable 语义（ADR 0643）。
- **预算化 JSON 结果具名**：同时供 tool JSON 与 `ToolResult` envelope 使用的截断状态，应与 JSON value 一起由 `JsonListBudgetResult { value, truncated }` 返回；执行预算操作使用 `cap_json_list`，调用方通过字段消费结果（ADR 0627）。
- **TypeScript 运行时词汇唯一化**：当某组字符串值只用于本模块的运行时遍历时，以 `as const` 值清单作为唯一 owner；不要再并列手写同值 union。函数若有意接受清单外的未知字符串并返回拒绝/回退结果，参数应标为 `string`；无跨模块消费者的类型和值清单不额外导出。
- **App shell 事件复用生成契约**：未变换的 app event payload 与闭合状态 enum 直接引用 Rust 生成 DTO/值清单；校验函数也按领域状态共用。只有进行 camelCase 映射、owner 关联或其它 renderer 归一化的事件才保留独立 view 类型（ADR 0696）。
- **Tool result 共用纯格式化**：跨 renderer 相同的 byte-size 与 percentage 表示使用 `formatByteSize` / `clampPercentage`；具体 payload 字段解析仍归各自 renderer（ADR 0697）。
- **Tool result 列表与滚动展示分层**：`ToolResultList` 负责分页和“显示更多”；`.tool-result-scroll-area` 负责共享限高滚动样式，不单独建无逻辑组件。各 renderer 可通过 `--tool-result-scroll-max-height` 调整本地高度；分页与滚动仍是不同职责（ADR 0704/0705）。
- **Tool result section label 只有一个样式 owner**：渲染器标题和计数统一使用 `.tool-result-label`，由 `ui/src/app.css` 定义。原 `.tool-card-count` 不只表示计数，不再用含义过窄的 card/count 名称或重复组件 CSS（ADR 0706）。
- **Tool result 正文预览共用样式**：文本/JSON 内容使用 `.tool-result-preview` 全局样式；renderer 只通过 `--tool-result-preview-max-height` 和 `--tool-result-preview-line-height` 表达确有差异的布局，不重复定义预览外观（ADR 0707）。
- **Tool result search row 共用样式**：Clipboard、FileSearch 与 WebSearch 的路径行、路径链接和次级说明统一使用 `.tool-result-search-row`、`.tool-result-search-path` 与 `.tool-result-search-detail`，由 `ui/src/app.css` 拥有视觉样式；领域字段与 row 内容仍由对应 renderer 拥有（ADR 0708）。
- **ToolRun 状态行共用布局**：后台 ToolRun 与定时 ToolRun renderer 复用 `.tool-run-row` 和 `.tool-run-id` 的基础布局/排版；仅后台 ID 因长度不定在本地追加 ellipsis（ADR 0710）。
- **Tool result 状态摘要行统一命名**：Agent 与 HTTP renderer 共用 `.tool-result-status-row` 的 flex/字号样式；各自的状态字段、badge 与关联信息仍归领域 renderer（ADR 0711）。
- **Tool result 窗口行基础样式统一**：系统显示器与窗口结果复用 `.tool-result-window-row`、`.tool-result-window-primary` 和 `.tool-result-secondary-value`；内容名分别使用 `display-name`、`window-title`，分辨率与窗口元信息分别使用 `display-resolution`、`window-meta`，不将分辨率或 condition/control type 称为 PID（ADR 0712）。
- **ExpandableContextCard header metadata 共用排版**：Builtin Tool、Tool root、MCP server 与 Skill 卡片使用 `.expandable-context-card-meta` 的 flex、间距、字号和换行规则；徽标与状态内容仍由各卡片 renderer 拥有，区别于 `.workspace-item-card-meta` 的卡片底部布局（ADR 0713）。
- **ExpandableContextCard 正文说明共用样式**：Builtin Tool family 和 Skill 的正文描述使用 `.expandable-context-card-description`，其字体、颜色、外边距与行高由 `ui/src/app.css` 拥有；具体说明文本分别由各卡片提供（ADR 0714）。
- **Tool result 状态/空结果正文统一称 message**：所有 renderer 的提示正文使用 `.tool-result-message`；compact、spaced、media 字号/间距与 error 色通过 modifier 明确表达，不把成功、等待或错误正文称为 empty（ADR 0715）。
- **可访问隐藏文本只维护一个样式 owner**：组件中的屏幕阅读器提示使用全局 `.sr-only` 工具类；调用组件只保留语义 label，不复制隐藏定位规则（ADR 0716）。
- **Tool result 元信息统一按结果角色命名**：所有 renderer 的元信息使用 `.tool-result-meta`；compact、input、media、memory、shell 排版差异通过具名 modifier 表达，样式由 `ui/src/app.css` 唯一拥有（ADR 0717）。
- **Untrusted record 识别共用一个 guard**：所有边界把未知值收窄到非空、非数组的 `Record<string, unknown>` 时使用 `isRecord`；字段语义校验继续留在各自 contract（ADR 0698）。
- **Tauri event channel 名使用生成目录**：UI 的 App、Agent、recording、Session 与 ToolRun event-name arrays 从 Rust `events.rs` 常量生成；payload map、运行时校验与 renderer 投影仍归各域 contract（ADR 0699）。
- **动态交互响应区分 wire 与 renderer view**：交互 envelope 的通用 `response` 保持 `unknown`；当某一交互类型在 UI 中具有稳定投影时，将 shape 命名为领域 view 并跨 reducer、controller、消息与组件复用。Ask 的答案/忽略结果统一为 `AskResponseView`，不在各层重复内联字段（ADR 0644）。
- **配置发现使用命名投影和领域 patch**：Model discovery 输入字段从 `ProviderDraft` / `ModelDraft` 派生，catalog 更新只以 `DiscoveredModelMetadataFill` 回传被填充的 model id 与 metadata。不要用 `Record<string, any>` 或开放 key/value map 表达已知 config 字段（ADR 0645）。
- **事件 handler 使用 channel→payload contract map**：Tauri event contract 的每个 channel 都有 `AgentEventPayloadMap` / 对应域 map；adapter callback 使用 `AgentEventListenerMap` 这类按 channel 映射的函数类型。UI transformation controller 使用 `satisfies` 校验它实际处理的子集，避免退化成 `Record<string, (event: any) => void>`（ADR 0646）。
- **事件 sink 按 channel 域和载荷角色命名**：持久 ToolRun 生命周期的 typed event stream 用 `ToolRunLifecycleEventSink` / `set_lifecycle_event_sink`；前台工具实时输出的 raw channel/payload 回调用 `set_live_output_event_sink`。两种 UI 通知来源的身份、payload 和生命周期不同，保持分离（ADR 0655）。
- **配置代次统一使用 `ConfigVersion`**：配置快照代次在 App apply plan、MCP 重连授权与配置操作结果中都使用 Common 的 `ConfigVersion`；跨边界或结果字段写明 `config_version`。ToolRegistry、MCP catalog、Skills catalog 的各自目录代次仍按 owner 分开（ADR 0656）。
- **Agent prompt index 只读取已发布目录**：内置工具索引从 `list_enabled_builtin_tool_definitions` 读取已发布 catalog；不要用 `ToolRegistry` 补空目录，因为它会绕开 enabled/deferred catalog 语义。prompt cache 的 `PromptCatalogVersions` 同时命名 global tool、MCP 与 Skills catalog 代次，`SchemaCache` 以单个 `catalog_versions` 值校验失效（ADR 0685）。
- **LLM 请求路由按 `RequestKind` policy 表达**：跨 crate 测试设置 primary model 时使用 `force_request_primary_for_test(RequestKind, model_id)`；不保留以 STT/vision 布尔值改写多个 request policy 的旧入口（ADR 0686）。
- **配置运行时计划分清映射与执行阶段**：`RuntimeConfigApplyPlan` 把配置变化映射为 live/restart runtime targets；`SettingsRuntimeApplyPlan`、`SettingsRuntimeApplyPhase`、`SettingsRuntimeApplyContext`、`SettingsRuntimeApplyTiming` 和 `SettingsRuntimeApplyOutcome` 等描述 settings 运行时应用的有序阶段、输入与结果，统一使用 `SettingsRuntimeApply*` family，并与 `SettingsRuntimeApplyCoordinator` 同名词根（ADR 0675）。
- **异步任务句柄标明执行职责且不保留恒真 optional**：Agent 有序 thought/reasoning chunk 队列由 `spawn_chunk_event_consumer` 创建，返回并直接持有 Tokio `JoinHandle`；不以泛名 `ConsumerHandle` 或始终为 `Some` 的 `Option<JoinHandle>` 隐藏任务 owner（ADR 0657）。
- **重试回调传领域状态而非语义布尔值**：LLM provider attempt 开始时用 `StreamAttemptOutputDisposition::{PreserveExisting, ReplaceExisting}` 表达如何处理前一次可见输出；chunk 和 attempt-start callbacks 分别称为 `StreamChunkCallback` 与 `StreamAttemptStartCallback`（ADR 0658）。
- **UI callback adapter 保留输入类型**：`typedCallbacks.ts` 只导出有实际调用点、按 `string`/`number`/`boolean`/`Event` 限定输入的 adapter；没有消费者的 `withAnyValue` 不保留为通用扩展入口（ADR 0659）。
- **Session 步数预算按统计作用域成对命名**：`SessionConfig.max_steps_per_run` 是每次 ReAct run 的步数预算；`max_steps_per_session` 是持久 Session 跨所有 run 的累计绝对 step number 上限。二者语义不同、各有 owner，不缩写成 `max_steps` / `session_max_steps` 或合成一个字段（ADR 0661）。
- **原始事件 envelope 保持 dynamic payload unknown**：Tauri listener adapter 只标注 `TauriEvent<unknown>` envelope；payload 在领域 mapper 验证前保持 `unknown`，不以 `any` 绕过 channel contract（ADR 0647）。
- **首次 session prompt 的历史上下文统一命名**：Agent `SystemPromptBuilder` 的输入以及内部预算渲染参数统一叫 `session_prompt_history`，与加载器和 `SessionPromptMessage` 的用途保持一致；模型提示正文中描述“conversation”的自然语言不强制替换（ADR 0648）。
- **Session compaction 的共享 prompt 常量标明领域**：Common 导出的 compaction 摘要指令统一叫 `SESSION_COMPACTION_SUMMARY_PROMPT`，由 Agent compactor 使用；指令正文为模型描述要总结的对话，继续保留自然语言 “conversation”（ADR 0649）。
- **Session 页面样式标识使用实体名**：当前会话主列与 SessionRail 的退出动画分别使用 `.session-column` 和 `session-rail-exit`；页面内部 class/keyframes 按组件职责命名，不用旧产品词 conversation（ADR 0650）。
- **Session 时间线按组件职责分名**：`SessionTimeline` 拥有加载/空状态边界，`SessionMessageTimeline` 渲染已有消息、活动与 ToolRun；二者负责不同的 presentation 状态，保留组件边界，不再用 `ChatMessageTimeline` 命名当前会话的消息 renderer（ADR 0653）。
- **Session welcome 只有一个展示 owner**：首屏空状态与空的 timeline projection 复用 `SessionWelcome`；由 `animated` 明确选择入场动画。`SessionTimeline` 仍拥有 loading/终态 gate，`SessionMessageTimeline` 仍拥有已投影消息的渲染（ADR 0709）。
- **Session 标题生成输入按真实数据命名**：`SessionTitleGenerationContext.user_messages` 传给 Agent `TitleGenerator::generate(user_messages)`，不得称为整个 `conversation`，因为 Memory 会明确过滤出用户消息并保持时间顺序（ADR 0651）。
- **Session prompt-history 配置由 Session 拥有**：首次 system prompt 的历史消息条数位于 `SessionConfig.prompt_history_limit` / `[session].prompt_history_limit`；Memory 配置只管理 Memory 功能，不保留已移出的 `session_window_size` 旧字段或 alias（ADR 0652）。
- **Memory fact write candidate 使用具名字段并共享策略转换**：从 `LlmFact` 解析出的来源引用与事实字段进入 `MemoryFactCandidate`，生产与测试都经过 `prepare_fact_candidates` 做同一套清洗、敏感值过滤和范围归一；不以 test-only 元组 alias 或第二份写入策略表达该阶段（ADR 0654）。
- **运行上下文按角色命名**：一起解析出的执行程序与工作目录使用 `ResolvedShellContext { shell, working_directory }`，解析动作命名为 `resolve_shell_context`，避免把两个不同含义的值作为位置 tuple 传给前后台执行路径（ADR 0628）。
- **跨组件提交输入共享类型 owner**：同一 chat submission attachment 在 Composer、页面、session controller 和 submit coordinator 之间复用 `chatAttachmentTypes.ts` 中的 `ChatImageAttachment` / `ChatFileAttachment`；仅用于预览的文件大小留在 `InputRouter` 的 `PendingChatFileAttachment`，历史消息 renderer 的宽松 `ChatBubbleAttachment` 继续独立（ADR 0629）。
- **结果类型由领域 owner 定义**：同一业务结果从数据库仓储传到异步 service/store 时，复用领域类型并只在边界调度执行；不要让中间层把具名对象拆回 tuple 再重建。
- **先查后设**：新增命名前先查是否已有同义词，避免重复词汇（如 `stt` 与 `asr` 语义不同，各归其位）。

## 产品与领域术语

以下是跨 Rust、Tauri 事件、Svelte 和用户文案的统一口径。代码、wire、数据库和配置统一使用 ToolCall/ToolRun 概念；历史 ADR 中的旧名称只用于说明当时的决策，不构成当前命名契约。

ReAct 按领域品牌大小写书写：类型与 enum variant 用 `ReAct`（如 `ReActEngine`、`ReActExecutionPhase`、`ReActLoopBarrier`）；snake_case 名用小写 `react`，lowerCamel 按词位遵循常规大小写（如 `reactExecutionPhaseStore`、`updateReactExecutionPhase`）。

应用能力可脱离当前 turn 持久运行时统一称为 `ToolRun`；当前源码、配置和 UI 不用退役的 `Job` 名称指代它。Windows `Job Object`、外部供应商异步 job 和 Memory outbox extraction job 各有独立语义，保留这些领域限定名称。

ToolRun completion lease 的 token 标识被 claim 的 ToolRun 或 completion result，不标识领取它的 consumer；跨 Tools runtime 与 ToolRun persistence 共用 `ToolRunLease`。

| 术语 | 含义 | UI 文案 / 代码边界 |
|---|---|---|
| 工具调用（ToolCall） | Agent/模型发起的一次工具调用；前台调用等待结果并进入当前 transcript | Agent/ReAct 使用 `ToolCall`；provider 的 `tool_call_id` 保持原格式 |
| 会话（session） | 用户与 Agent 的对话及其前台 ReAct 运行上下文 | UI 直接称“会话”；前台工具调用在会话内呈现 |
| 会话步骤状态（`SessionStepStatus`） | 持久 `session_steps.status` 与 live `StepInfo.status` 共用的闭合生命周期值：pending、running、completed、failed、cancelled、unknown | Common 定义并生成给 UI；SQLite 列仍存 snake_case 字符串，Memory 读入时严格解析。`ToolStepOutcome` 是 finish 操作的终态输入，不替代完整状态（ADR 0725） |
| 会话运行（SessionRun） | `SessionSupervisor` 调度或直接准入的一次会话执行；`SessionRunEngine` 运行完整 ReAct 循环 | handler、admission、permit、lease 和 actor claim 均显式标注 `SessionRun`；直接运行的准入等待项为 `DirectSessionRunAdmissionWaiter`，具名区分注销 ID 与取消令牌；ReAct loop 的输入、重放、输出以 `ReActRun*` 标名 |
| 工具运行（ToolRun） | 脱离当前 turn 持久运行、可取消并产生生命周期事件的工具执行 | 后端/IPC/数据库使用 `tool_run`、`tool_runs`；ID 前缀为 `toolrun-`。工具查询与 completion notification 共用 `ToolRunStateView` 的状态基础 JSON；`background_wait`、kind/source 与 delivery envelope 仍由各调用者按自身职责添加（ADR 0700） |
| 工具结果（ToolResult） | 一次工具调用的成功/失败输出及执行元数据 | `ToolResult.truncated` 属于结果级元数据；正文中的 `truncated` 属于具体工具输出 shape。正文标记为 true 时两层必须同步；正文没有该字段时允许只保留结果级标记（ADR 0701）。Agent observation 的执行 outcome 只由 `ToolResultEnvelope.outcome` 承载；UI 在展示边界映射成卡片状态。历史 `session_steps.status` 是恢复时的持久投影，不是第二个 event outcome 来源（ADR 0719） |
| 后台工具运行（background ToolRun） | 工具调用选择后台执行后启动的持久运行 | 通过 `ToolExecutionMode::Background` 启动；UI 显示“后台任务” |
| 定时工具运行（scheduled ToolRun） | 由时间或依赖触发的工具运行 | 仍由 `schedule` 工具负责设置触发条件；UI 显示“定时任务” |
| Memory live consumer handoff | 把已准备的 Memory event receiver future 一次性交给 ApplicationRuntime task registry 的阶段值；注册成功后才产出 `MemoryReady` | 类型为 `MemoryLiveConsumerHandoff`；由 `MemoryStartup::prepare_live_consumer` 创建，通过 `register_consumer_with` 注册，不代表 `ToolRun` 或已运行的 JoinHandle |
| 音频输入管线（`InputPipeline`） | `haven-input` 对麦克风采集、VAD 与采集循环的唯一 owner；不拥有转写、provider fallback 或 App 的录音 UI 状态 | 常规采集用 `start_capture` / `stop_capture` / `cancel_capture`；固定时长采集用 `capture_for`；回调契约为 `InputEventHandler`。App 与 Tools 的字段统一叫 `input_pipeline` |
| VAD 状态事件 | App 的 `VadStatusEvent` 是 `recording:vad_status` 的唯一 wire DTO；`signal` 与 `state` 保留开放字符串以允许新增值 | IPC 生成器导出 Rust DTO；UI `VadStatusPayload` 直接引用生成类型，`mapRecordingEvent` 仍在唯一事件入口处理畸形输入（ADR 0694） |
| 录音结果（`RecordingResult`） | `haven-input` 拥有的固定格式采集结果：16 kHz 单声道 PCM、停止原因、时长和采集错误 | 通过 `RecordingResult::encode_wav()` 编码；它与 Tools 持有的 `RecordedAudio`（已登记 WAV 资产结果）不是同一层结果 |
| 持久会话事件（`SessionEvent`） | `session_events` 中 append-only 的恢复与回滚权威记录 | Memory 持有 event row、sequence 与存储；`TranscriptRecord` 是其版本化 payload，不是第二条日志 |
| 会话消息（`Message`） | 从已提交 transcript 投影出的持久消息行，不是恢复来源 | UI reducer 的 `SessionMessage` 是独立 renderer shape；`TranscriptProjection` 是从事件流派生的读取视图，含 `canonical_messages` 与 `react_rounds` |
| 运行事件（`AgentEvent` / `SessionSupervisorEvent`） | 进程内的 Agent 输出或 Supervisor 生命周期通知，不作为 durable transcript | durable 写入仍使用 `SessionEvent`；UI `session:lifecycle` 是 App 投影契约 |
| 交互请求（`InteractionRequest`） | Agent 内用于 Ask/Confirm/ScheduledConfirm 的状态与继续执行数据；可包含不应发送到 UI 的工具输入和授权 receipt | `InteractionOwner` 只负责进程内 owner routing；生成的 `InteractionRequestedEvent` 是 App wire projection，前端 `InteractionRequest` 是经过校验的 camelCase view |
| 工具实现、定义与清单 | `ToolHandle` 是单个共享可执行实现；Common `ToolDef` 是 provider-neutral 的模型调用定义；LLM `LlmToolDefinition` 是转换后的 `{type, function}` 请求中间形状；`ToolManifest` 是 Tauri/UI 的 identity、policy 与 presentation projection | `ToolDef → LlmToolDefinition → provider adapter wire`；`ToolRegistry` 持有可执行实现；`ToolCatalogSnapshot` / `OperationCatalog` 是模型可见目录投影；`OperationRegistry` 管已安装、deferred 与 session operations。目录可见不等同于授权 |

定时工具运行的 `mode` 只作为行为说明：`tool` 显示“调用工具”，`continue` 显示“继续会话”。运行状态统一显示“待执行 / 运行中 / 已完成 / 失败 / 已取消”；原始枚举值只留在 wire、日志或调试详情中。

`SessionRun` 是 SessionSupervisor 的会话执行/准入单位；`ToolRun` 是可脱离当前 turn 持久运行的工具工作单元，二者不共享身份或生命周期。`ReActRunInput`、`ReActRunReplay`、`ReActRunOutput` 是 ReActEngine 一次循环的调用数据，不新增持久 run 实体。准入计数器、RAII permit、直接运行 lease 与 actor claim 有不同释放点，保持独立结构并按 owner 命名（ADR 0635）。

`ToolExecutionMode::Foreground/Background` 表示调用执行方式；持久 `ToolRunKind` 只有 `Background/Scheduled`。会话是对话实体，不是 `ToolRunKind`，不得把“会话”塞进任务类型映射。

代码中，**session** 用于指向持久会话实体及其运行状态；**conversation** 仅在描述自然语言交流内容、历史文本或模型上下文时使用，不用来命名会话实体的状态和 UI 组件。UI 的 `SessionMessage` 是当前会话 reducer 的消息形状；controller 直接消费 `getMessages()` 返回的 `SessionMessage[]`，不再声明私有子集并强制 cast。`sessionTimeline.ts` 接收该类型并投影为 `SessionTimelineItem`，不得再声明一份宽松的平行消息结构。只有字段约束或展示职责确实不同的组件专有输入（如允许文件路径的 `ChatBubbleAttachment`）才在组件内保留，并用组件/视图角色命名。

Tauri listener 的通用 `TauriEvent<T>` envelope 由 `contracts/tauriEvent.ts` 唯一声明；Session、ToolRun、Agent、App 和录音 contracts 只定义各自 payload 与转换，不重复定义同形 envelope。

Memory `partial_messages` 中尚未提交到 canonical transcript 的流式文本称为 `PartialMessageCheckpoint`；读取结果以 `content` 与 `updated_at` 字段表达，不把草稿文本冒充为已持久化 `Message`。

Agent 从权威 `TranscriptRecord` event log 得到的派生视图统一由 `TranscriptProjection` 表达：`canonical_messages` 是发给模型的 provider-neutral transcript，`react_rounds` 是 Agent 步骤/工具恢复视图。二者同源并可同次计算，但有不同消费者与约束，不互相替代，也不合并成一种消息 shape。

ReAct response hook 与分类器共享 `ResponsePolicyInput`；分类器返回 `ResponsePolicyDecision`（接受、结构参数重试或可恢复失败）。response cycle 执行策略后再返回 `ResponseCycleOutcome`，二者是不同阶段，不合成一个状态类型。实现模块使用 `response_policy.rs`，不再用只覆盖 retry 的 `retries.rs` 命名整个分类器（ADR 0637）。

SessionReducer 的当前 run 结束提示统一为 `SessionRunEndNotice { sessionId, status, reason }`，状态范围从 generated `SessionStatus` 派生为 paused/completed/error。活动错误不再另存一份 `SessionError`；生命周期 reducer 用单一 `session/run-ended` action 同步会话状态和提示，继续生成成功后用 `session/run-end-notice-cleared` 清理。历史错误原因映射仍按 session 单独保留（ADR 0638）。

共享 UI 状态色使用 `StatusTone`；`NotificationType` 是从中提取的信息/成功/警告/错误通知子集，`StatusBadgeTone` 排除没有 Badge 样式的 `tool`。ToolRun 完成 toast 再从 `NotificationType` 提取其实际支持的 `info/success/error`；保留组件和通知的角色名，不重复手写同义字面量（ADR 0639、0642）。

录音 overlay controller 的命令注入范围命名为 `RecordingCommandName`，由 generated `TauriCommandName` 显式提取 `start_recording`、`stop_recording` 与 `cancel_recording`；它是 controller 的窄能力边界，命令值仍由 generated IPC contract 持有（ADR 0640）。测试 mock 直接从 controller dependency 的 `invoke` 字段取得函数类型，不复制一份命令 union。

Tools 文件名与全文搜索共用 `FileSearchResult`；匹配项由 `results` 表达，有限扫描或结果上限则由独立的 `truncation_reason` 表达。匹配数据与搜索完整性是不同结果维度，不能只根据 tuple 位置恢复。

Memory summary extraction marker 的序列化值解码为 `DecodedSummaryExtractionMarker { session_id, attempt, next_attempt_at_ms }`；它是解析出的持久值，不等同于分页查询对消费者暴露的 `SummaryExtractionMarker` 或其重试状态。

LLM `ModelDirectory::resolve_client` 对执行请求返回 `ResolvedModelClient { model_id, client }`：model identity 用于并发/限流/健康状态关联，`LlmClient` 是实际执行请求的 adapter。非执行目录缺失时使用默认 adapter 的 `select_client` 保持独立语义。

Tools 对结构化输出中 Ask 与通知 side-channel 的解析结果分别使用 `AskSignal` 和 `NotificationSignal`；前者保留可选 question 与选项列表，后者以 `Option<NotificationSignal>` 表示是否请求通知。汇总传递给 Agent 的 `ToolSignals` 形状继续由其独立的 side-channel 契约拥有。

跨端枚举的允许值以 generated IPC contract 为单一来源；UI 可以为这些值维护展示标签，但选项数组应从生成值派生，并让边界/事件 contract 直接引用生成类型，不另手写相同 union。

`LlmCallKind` 的 `agent` / `media` / `tool` 是闭合的本地 usage 类别；runtime 输入、live `agent:usage` event 与恢复 DTO `LlmUsageRecord` 共用生成 enum。SQLite `llm_usage.call_kind` 原始列仍以字符串存储，在 Memory 读取边界必须解析为该 enum，不把 SQL 表示扩散成第二个 wire owner（ADR 0271、0723、0724）。

Tauri command 名与 request/response 类型由 `generatedCommands.ts` 从 Rust handler 生成；`contracts/commands.ts` 只追加 reviewed boundary/security metadata，并用 `Record<TauriCommandName, CommandContract>` 保证每个生成 command 恰有对应审阅项。

跨 crate 的同字段 DTO 先按 owner 和用途判断是否合并：Memory `SessionMessageText` 是存储查询返回的纯文本消息行；Agent 私有 `SessionPromptMessage` 是组装首次 session prompt 的输入。它们通过显式转换跨边界，不应让 Memory 依赖 Agent，也不应让 Agent 的 prompt 类型成为 Memory 的规范类型。

同一领域类型跨 runtime 与 wire 边界时，只有序列化格式、字段策略或演进 owner 确实不同才保留两个类型，并在名称中标出边界角色。当前 Tools `ToolRunKind` 是执行运行时分类；App `ToolRunKindDto` 是 IPC/event DTO 枚举，二者值相同但 owner、Serde 与向前演进责任不同。

Tools manifest 的 generated `ToolManifest` 是 Rust snake_case wire DTO；renderer parser 投影出的 camelCase 结构叫 `ToolManifestView`。source、represented source、catalog group 与 risk level 复用 generated 闭合枚举；`presentation.renderer` 保持开放扩展字符串，MCP 使用固定 `mcp` 值，server identity 由 root 字段表达，未知 renderer 落到通用 JSON renderer（ADR 0669、0703）。

严格生成的 IPC `LlmConnectionReport` 与容忍缺失可选显示信息的 `LlmConnectionReportView` 分属 wire 与 renderer 视图；共享的 status/reason 枚举直接引用生成契约，归一化函数负责将不可信返回值投影为 view。

交互的 generated `InteractionOwner` 保持 snake_case wire shape；App contract 映射出的 camelCase `InteractionOwnerView` 供 reducer 路由和用户操作使用，回发命令时再由 `interactionOwnerToWire` 转回 generated wire 类型。

## 架构角色词汇

类型后缀不是装饰词：它必须说明对象的职责。新增和重命名类型按下表选用；存量不一致项在全项目术语审计中逐域处理，不做机械批量替换。一个类型若同时符合多个角色，应先明确它真正拥有的职责，再决定保留组合名还是拆分。

| 词汇 | Haven 中的约定含义 | 不应用来表示 |
|---|---|---|
| `Port` | 某层消费的窄能力接口；由上层组合根提供适配。 | 具体实现或任意参数对象。 |
| `Adapter` | 在两个稳定边界间转换数据/调用并实现目标 port。 | 领域规则的唯一 owner。 |
| `Client` | 对外部 provider、服务或协议端点执行 I/O 的调用端。 | 本地数据目录或纯配置。 |
| `Provider` | 为调用方提供某类可替换能力/数据的来源。 | 纯转换器；此类使用 `Adapter`。 |
| `Router` | 根据显式用途/能力选择目标 provider、模型或执行路径。 | provider 线协议实现或通用请求生命周期 owner。 |
| `Resolver` | 从已知输入和 owner 中解析一个值/身份/候选；不承载授权副作用。 | 数据字段纯映射（`Mapper`）或权限裁决（`AuthorizationEngine`）。 |
| `Loader` | 按请求从已知来源装入数据/能力，并说明目标作用域及失败语义。 | 仅列举/描述目录，或绕过 owner 直接执行目标能力。 |
| `Store` | Rust 后端的具名数据读写边界；持久化写入的一致性/事务由它或它委托的唯一 owner 定义。Svelte 侧沿用生态术语，`xxxStore` 表示响应式状态容器，不表示持久化。 | Rust 后端的纯缓存或通用 SQL 连接；前后端不能因同一个后缀而假定职责相同。 |
| `Repository` | Memory crate 内按持久实体组织的实现模块；跨层接口和事务 owner 优先使用 `Store` 术语。 | 与 `Store` 并列、职责不明的第二套持久化端口。 |
| `Registry` | 按稳定 key 管理权威条目/实现，提供注册、替换与查找；必要时拥有顺序、唯一性和版本。常见例子：`ToolRegistry`、`ModelRegistry`、`SkillRegistry`。 | 只展示描述或搜索结果的目录投影。 |
| `Catalog` | 供发现、列举、描述或筛选的能力/资源目录；读取目录本身不授予执行权。 | 已激活的执行集合或执行授权决策。 |
| `Index` | 为查询建立的派生查找结构；不独立拥有源数据生命周期。 | 注册/删除业务实体的权威入口。 |
| `Snapshot` | 带明确范围/版本的不可变时点视图。 | 长期可变 owner，或含糊的“当前对象”。 |
| `Projection` | 从事件或权威实体推导出的只读视图。 | 恢复/回滚的第二真源。 |
| `Cache` | 可丢弃并可重建的性能副本；必须能指出失效条件和 owner。 | 持久权威状态。 |
| `State` | 一个明确作用域内的可变状态模型；名称前部标出 session、run、页面等作用域。 | 无范围说明的全局杂项容器。 |
| `Context` | 一次操作/请求所需的显式输入与依赖视图；生命周期短且范围可读。 | 持久状态 owner 或不受控的服务集合。 |
| `Policy` | 可审查的约束、分类或决策规则。 | 只提取/映射字段、但不决定策略的 helper。 |
| `Plan` | 已校验、尚未生效的拟执行变更或动作列表。 | 已提交的状态或副作用结果。 |
| `Service` | 一个领域能力的调用面；编排该领域规则与 store/port，不做无边界的对象汇总。 | 仅转发另一对象、却不说明其 owner 语义的总入口。 |
| `Manager` | 管理一组资源的创建、替换、重连或生命周期。 | 只有组合/转发职责的 façade。 |
| `Facade` | 对内部多个窄接口提供稳定的组合调用面；不隐含额外状态权威。 | 另一份业务状态 owner。 |
| `Coordinator` | 按明确顺序协调多个既有 owner 的转换；不得暗中复制其状态。 | 通用工具箱或第二个生命周期 owner。 |
| `Gate` | 串行化临界区的同步原语，例如共享 mutex；字段名表示锁本身。 | 同时拥有持久编辑与运行时 prepare/publish 流程的协调对象（`Coordinator`）。 |
| `Runtime` | 已应用、供执行路径使用的活跃配置/资源集合；`Prepared*` 表示尚未发布的候选。 | 一次请求的临时数据包或只读快照。 |
| `Engine` | 执行有明确输入/输出的算法、循环或判定过程。 | 负责装配所有依赖的组合根。 |
| `Worker` | 消费队列/outbox 或周期任务的后台处理循环。 | 面向单次调用的同步服务。 |
| `Supervisor` | 管理 actor/worker 的启动、注册、恢复和退出边界。 | 单个会话的业务状态本身。 |
| `Owner` | 对一个可变生命周期/状态作出唯一串行决策的对象。 | 只负责广播或无状态转换的辅助器。 |
| `Handle` | 对单个资源的共享引用；名称应标出所引用资源，克隆会延长底层对象的存活时间。 | 资源集合、注册目录或生命周期管理器。 |
| `Executor` | 对已准入/已授权的目标执行一次操作，并拥有执行结果分类。 | 选择目标、授予权限或构造策略。 |
| `Handler` | 接收一个命令、事件或协议入口并转交给领域 owner。 | 跨多个页面/领域的大型编排器。 |
| `Controller` | UI 或应用入口的异步流程编排，依赖通过参数显式传入。 | 领域持久化 owner 或展示组件。 |
| `Reducer` | 由 action/event 和旧状态确定新状态的转换函数/对象。 | 网络、数据库或通知副作用的 owner。 |
| `Mapper` | 在明确边界转换字段/类型；每个 wire mapper 有唯一登记点。 | 负责业务决定或异步生命周期的 service。 |
| `Builder` | 用多个输入装配一个结果，且调用方能看出构造范围。 | 发布/应用该结果的 coordinator。 |
| `Factory` | 按显式参数/策略创建某类实例；若只封装单一 DTO 组装，优先叫 `Builder`。 | 运行期资源管理器。 |
| `Event` / `Command` | `Event` 表达已经发生的事实；`Command` 表达请求执行的意图。 | 彼此混用的通用 payload。 |
| `Bridge` | 仅为既有协议/生态术语保留；新跨边界类型优先使用更具体的 `Adapter`、`Mapper` 或 `Port`。 | 与这些角色并列但无独立语义的通用类型后缀。 |

### 函数动词

同一调用链中按**可观察语义**选动词；不能只为避免重名而替换同义词。下列规则适用于 Rust 方法与 TypeScript 导出函数，测试名按对应测试约定表达行为。

| 动词 | 含义 |
|---|---|
| `get` / `find` / `list` | 按稳定 key 取单项 / 按条件搜索 / 读取集合；缺失语义由返回类型体现，`get` 不暗示一定存在。 |
| `read` / `fetch` / `query` | 从本地资源或 owner 读取内容/当前投影 / 执行跨边界 I/O 读取 / 对本地或持久数据执行有条件查询。 |
| `load` / `restore` / `resume` | 读入运行态 / 从持久来源重建运行态 / 从保存的会话位置继续执行。 |
| `build` / `create` / `new` | 纯装配派生值 / 建立具有业务身份的实体或资源 / 普通构造函数。 |
| `prepare` / `apply` / `publish` | 生成未生效候选 / 对 owner 应用变更 / 将已提交版本暴露给消费者。 |
| `append` / `commit` / `persist` | 向追加式日志写入 / 原子提交一组变更 / 将状态写入持久存储。 |
| `register` / `activate` / `enable` | 加入注册表 / 纳入当前执行作用域 / 允许既有能力使用；三者不能互代。 |
| `update` / `replace` / `clear` / `delete` / `remove` | 局部修改 / 整体替换 / 清空集合 / 删除持久实体 / 从某个运行集合移除。 |
| `drain` / `pop` / `take` | 取出并清空队列或集合 / 移除并返回一个队头元素 / 转移或清空某个可选 owner 的值。 |
| `execute` / `run` / `handle` / `process` | 执行一次具名操作 / 推进一次流程或后台任务 / 接收并路由入口 / 消费或转换输入。 |
| `resolve` / `authorize` | 按 owner 与输入解析目标/契约 / 由安全 owner 作出允许、拒绝或确认决策。 |
| `emit` / `publish` / `send` | 发出事件 / 发布已提交状态 / 向外部端点或收件人发送消息。 |
| `map` / `project` / `normalize` / `parse` | 结构转换 / 从权威源派生视图 / 将宽松输入规整为契约 / 解析文本或 wire 格式。 |

Hotkey 领域中，`KeyCombo::has_modifier` 表示修饰键位掩码判断；`KeyCode::display_name` 表示面向用户的显示标签，区别于输入字符串 parser 与稳定键身份。

VAD 模型输出语音概率的推理入口使用 `infer_speech_probability`；检测器接收该值时使用 `observe_probability`，明确它会根据概率推进检测状态并产出 `VadSignal`。避免在同一条职责链里用通用 `infer`、`process` 和 `prob` 隐去输入输出含义。

Input capture 内部的 engine owner 和命令分别使用 `CaptureEngine`、`CaptureEngineCommand`、`CaptureEngineHandle`；活动 ring 的直接读取叫 `drain_buffered`，隐藏 mutex 共享实现。`Resampler` 的状态式转换入口使用 `resample` / `resample_into`，区分音频变换和泛化的 `process`。

`InputPipeline` 的 ring capacity 配置入口叫 `set_ring_buffer_capacity_secs`，音频参数替换叫 `update_audio_config`。ring capacity 当前只在下一次 capture engine spawn 时生效；运行中 engine 不会因设置变更而 resize（ADR 0634）。

Common 媒体探测 helper 和变量以 `mime_type` 表示 MIME 字符串，探测/扩展名映射函数使用 `*_mime_type`；`MediaProbe` 的 Rust 与 Serde 字段统一为 `media_kind`，MIME 字符串为 `mime_type`，不为旧 `media_type` key 保留别名。此规则仅针对探测结果；消息附件、受管媒体资产及 provider wire 中表示 MIME 字符串的 `media_type` 按各自现行契约保留。`DetectedMediaKind` 是 bytes/extension/MIME 推出的粗分类（含 `Unknown`）。能力规划的 `MediaModality` 没有 `Unknown`，继续表示模型能力合同，不与检测失败分类合并。

数据库或领域查询即使按 session、subject、tag 等条件筛选，只要结果是零到多条实体，也使用 `list_*`（条件检索可使用 `find_*` / `search_*`）；`get_*` 留给单实体读取。缓存接口按稳定 cache key 读写一个缓存槽时仍可使用 `get_*`，即使槽内缓存的是集合。

当前进程驻留集合与持久历史必须明确区分：运行态使用 `list_runtime_sessions`；从数据库读取的会话记录使用 `list_persisted_sessions` 或 `list_session_history`。缓存名跟随缓存的查询结果（`session_history_page`），不可复用运行态会话集合的名称。批量持久删除使用 `delete_all_sessions`；Supervisor 内部对驻留 actors、队列和授权状态的清理使用 `clear_session_runtime_state_locked`。

读取接收者自身当前状态时使用名词式 accessor：单一主状态用 `state()`，同一 owner 暴露多个状态视图时用带领域名的 accessor（如 `vad_state()`）；需要读取一个时点值而非订阅后续变化时使用 `snapshot()`，计量值显式带单位（如 `durationSeconds()`）。`get_*` 保留给按 key 读取值，避免 `get_state()` 这类无查询键的泛化动词。

这些词汇用于审计和迁移，不授权把不同的状态 owner、错误语义、事务边界或安全策略合并。发现名称相似时，先比较不变量、生命周期、失败行为和真实消费者；只有职责与权威来源相同才合并，否则保留边界并改成能表达作用域/角色的名称。

---

## 1. 后端 Rust

### 文件名 / 模块名
- **snake_case**，如 `stt.rs`、`openai_responses.rs`、`scheduled_tool_run.rs`。
- 目录即模块：`crates/tools/src/builtin/`、`crates/memory/src/repositories/`。
- crate 统一 `haven-{name}`：`haven-agent`、`haven-common`、`haven-memory`、`haven-tools`、`haven-llm`、`haven-input`、`haven-mcp`、`haven-skills`。

### 标识符
- 类型 / 枚举 / trait / 结构体 → **PascalCase**：`Modality`、`Intent`、`LlmConfig`。
- 函数 / 方法 / 变量 / 字段 / 模块 → **snake_case**：`fn detect_intent`、`stt_default_base_url`。
- 常量 / 静态 → **UPPER_SNAKE_CASE**：`MAX_SPEAK_CHARS`、`IMAGE_GEN_KEYWORDS`。
- 构造 `pub const fn as_str` / `new` 保持惯例命名。
- 跨模块返回多个具有稳定领域含义的值时使用具名结果结构体，字段名直接表达各自角色；不要让调用方通过 `.0` / `.1` 解读分类、版本时钟和 MIME 等语义。短生命周期局部组合、迭代器键值等仍可使用 tuple。不同分类空间即使都包含 `kind` 字段，也按 owner 命名具体结果（如 `FileClassification` 与 `ManagedMediaClassification`），不为形似而合并 enum。
- 可复用的领域复合 key 使用具名结构体和闭合枚举表达各部分含义；不要用 tuple alias 加字符串标签编码固定身份。临时局部键值组合仍可用 tuple。

### 类型角色后缀

后缀表示类型的主要责任 owner，不是所有可能行为的清单；不要只因同处一个 crate 或拥有相似字段就统一后缀。类型承担多个主要 owner 时，先记录具体消费者，再判断是否拆分或改名。

| 后缀 | 主要职责 | Haven 示例 |
|---|---|---|
| `Engine` | 执行领域算法、反应循环或媒体处理过程 | `ReActEngine`、`SessionRunEngine`、`VadEngine`、`FileSearchEngine`、`CaptureEngine` |
| `Runtime` | 持有进程内活动状态、长生命周期资源或运行任务 | `ApplicationRuntime`、`MemoryRuntime`、`ToolRuntime` |
| `Store` | 通过持久化边界读取或写入领域数据 | `SessionStore`、`MemoryFactStore`、`MemoryRecallStore` |
| `Registry` | 按身份索引并提供目录、catalog 或可选规则集合 | `ToolRegistry`、`SkillRegistry`、`ModelRegistry`、`ManagedAssetRegistry` |
| `Manager` | 创建、替换、重连或关闭外部/子系统资源 | `McpManager`、`VenvManager` |
| `Service` | 组合多个 owner 完成应用或领域用例 | `ConfigService`、`MemoryService`、`MessagingService`、`ToolRunService` |
| `Facade` | 组合窄接口提供统一调用入口，不取得被组合 owner 的生命周期 | `ToolsFacade` |

同一子系统可以合理同时包含多个后缀角色：`McpManager` 管连接，`McpClient` 管单个连接的协议交互；`SkillRegistry` 提供发现结果，而 `VenvManager` 管执行环境。上述区别属于资源 owner 和生命周期差异，不需为了词形一致合并。

### 缩写大小写规则
- **类型名**中缩写用 PascalCase：`SttProvider`、`OcrEngine`、`TtsProvider`。
- **函数 / 文件 / 变量 / 字段**中缩写当作整词用小写：`stt.rs`、`tts.rs`、`ocr.rs`、`stt_default_base_url`。
- 一个概念只用一个缩写词，禁止换用：语音转文本统一 `stt`，OCR 统一 `ocr`，文本转语音统一 `tts`。
  - 例外：`asr` 是用户输入的关键词（意图识别 vocabulary，与 `ocr` 相邻），属于**输入信号**，不是 provider 模块名，不并入 `stt` 词汇表。二者语义不同，各归其位。

### ID 规范
实体 ID 统一 `{prefix}-{uuid32}`，一律用 `haven_common::types::new_id(prefix)`，禁止手拼。完整前缀表见 `AGENTS.md`.

- ToolRun 身份字段在 Rust 类型、事件与 JSON 结果中统一命名 `tool_run_id`；前端 IPC/view DTO 只在边界映射为 `toolRunId`，不再接受或输出泛名 `id` 别名。数据库物理列 `tool_runs.id` 保持不变，Repository 行字段命名 `tool_run_id`。Tauri event envelope 的数值 `event.id` 是另一实体，继续保留原名。

---

## 2. 前端 Svelte 5

### 组件（`.svelte`）
- 文件名 = 组件名，**PascalCase**：`ApiKeyDialog.svelte`、`ToolResultCard.svelte`。
- 由 `.svelte` 文件隐式定义组件，不额外命名导出，避免名不符文件。

### 模块（`.ts`）
- 工具 / 状态模块 → **camelCase**：`streaming.ts`、`voiceSubmit.ts`、`markdownRenderer.ts`、`sessionStatus.ts`、`modelRoles.ts`。
- 前端逻辑模块、测试、Vite/Svelte 配置和 Node 工具脚本统一使用 TypeScript；Node 工具脚本使用 `.ts` 并由固定 Node 工具链直接运行。
- Svelte 组件脚本的目标形式为 `<script lang="ts">`；存量组件按域分批迁移，迁移时补齐参数、状态和 DOM 引用类型。
- UI 源码不新增 `.js` / `.mjs` 独立实现模块；迁移完成后，Svelte 组件也不再保留普通 `<script>`。
- 主要导出 Svelte store 的模块 → `xxxStore.ts`：`themeStore.ts`、`syncStore.ts`（`syncStore.ts` 导出同名的 `syncStore` 辅助函数，名随主导出）。
- 聚合 store 桶文件保留 `stores.ts` 命名（导出 `sessionStore`/`toolRunStore` 等命名导出）。
- IPC DTO 的前端 alias 放在对应领域的 `contracts/` 模块，已知字段从 generated command type 派生；确需开放扩展时显式叠加索引签名，不把稳定响应整体退化为 `Record<string, unknown>`。`contracts/` 中被生产消费者使用的重导出是领域导入 façade，不拥有第二份 wire shape；未使用的同名 alias 应删除。仅做状态判断/标签映射的 UI utility 直接导入 generated enum/value，不再导出同名的无变更 alias；有独立 renderer shape 或领域角色时才定义前端类型。模块内部若另声明只改名、不增加形状约束或角色语义的类型 alias，也直接使用规范 owner 类型；不要用局部别名制造第二套词汇。不同生产者若传递同一个 generated command payload（例如性能指标 aggregator），也直接引用该 contract；只有 shape 或语义角色变化时才定义 renderer 类型。
- 同一通用依赖注入类型若被 feature 局部 alias 仅改名（如 `TauriCommandInvoke`），controller dependency 与 command wrapper 参数直接使用 generated owner；只有缩窄能力或输入/输出约束时才另定义专用 invoker port。
- 多个组件共享的表单字段、回调输入或字段联合类型由领域类型模块单一定义；持久配置草稿用 `Draft`，仅供编辑器使用的临时表单用 `Form`，供回调判定的输入用 `Input`，避免不同生命周期共用含糊名称。
- 从共享 discriminated union 提取给不同 reducer 使用的私有动作子集，以 `<Domain>ReducerAction` 标明实际消费者；`Action`、`Props` 等短名只在明确的组件或模块作用域内作为私有局部类型使用。
- 常量 → **UPPER_SNAKE_CASE**：`SESSION_STATUS_VALUES`、`COLOR_MAP`、`ROLE_KEYS`。
- 局部变量 / 函数参数 → **camelCase**：`newKeyValue`、`reasoningOpen`、`ctxMenuItems`。

### 路由
遵循 SvelteKit 约定：`+page.svelte`、`+layout.svelte`。当前工作区只保留根路由，设置、工具和记忆通过根路由的 `?tab=` 查询参数切换，不保留旧的目录路由。

---

## 3. 跨层边界（Rust ↔ 前端）

| 域 | 后端 | 前端 |
|---|---|---|
| 标识符 | snake_case | camelCase |
| 事件字段 | `session_id`、`step_number` | `sessionId`、`stepNumber` |
| Tauri 命令 | snake_case（`get_log_info`） | invoke 时转换 |
| 实体 ID | `{prefix}-{uuid32}`（统一） | 原样透传 |

- **前端只在边界转换**（invoke 调用 / 事件监听处），内部统一 camelCase。
- **Reactive / 数据字段**（如 DB 行字段、测试 fixture）允许保留后端 snake_case，不强行改前端内部就 camelCase 化。
- 不要在调用链深处出现重复的手工 snake↔camel 转换；将来集中收敛到命令/事件封装层。
- **会话恢复统一叫 `resume`**：从历史打开会话、崩溃恢复、DB→气泡重建均用 `resume`（如 `get_session_for_resume`、`get_latest_session_for_resume`、`sessionResumeTargetStore`、`buildResumeMessages`、`SessionResumeResponse`）。会话恢复目标的类型带 `Session` 作用域前缀；禁止再用 `review` 指代该流程（`preview` 预览、code review 注释、工具 capability `"review"` 除外）。

---

## 4. 名词单复数

- **容器 / 集合 / 表 / 目录 / 仓库** → **复数名词**：`sessions`、`messages`、`tool_runs`、`facts`、`session_steps`、`memory_embeddings`、`modelCards`、`messages`。
- **单一实体 / 单行元素** → **单数**：`session`、`message`、`tool_run`、`row`、`card`、`msg`。
- **不可数 / 质量名词** 保持单数：`usage`、`audio`、`video`、`text`、`schema`、`kv_store`（复合词不数）。
- **前端 store 变量** 按承载实体命名（`sessionStore`/`toolRunStore` 可承载数组/对象，名字取实体单数，属约定）。
- **派生集合结果** 用「实体＋复数」或复数词，避免用裸形容词承载集合：写 `selectedSessions`、`filteredMessages`、`remainingMessages`、`keptExistingMessages`，不写 `selected`/`filtered`/`remaining`/`keptExisting` 指代数组。
- store `update` / `filter` / `map` 的回调单元素参数用单数短名（`m`/`x`/`row`/`card`/`t`），保持单数语义。
- **文件名 / 结构体名保持一致**：一个文件一个实体时文件名单数；实体本身为集合资源时文件名随结构体用复数：`files.rs` ↔ `FilesTool`、`tool_runs.rs` ↔ `ToolRunsTool`（启动名 `"files"`/`"tool_runs"`）。不可数域用单数：`memory.rs` ↔ `MemoryTool`（启动名 `"memory"`，覆盖 facts + items）。事实实体统一使用 `Fact` / `facts`，后台编排统一使用 `MemoryWorker`。
- 仓库 / 表名按所管理实体的复数命名，与其承载集合一致：`sessions.rs`、`messages.rs`、`facts.rs`、`session_steps`。
- 不可数名词文件（`usage.rs`、`media_audio.rs`、`text.rs`、`schema.rs`、`memory.rs`）保持单数。

> 该漂移已对齐：`scheduled_tool_run.rs`↔`ScheduleTool`、`env.rs`↔`EnvTool`（原 `env_var.rs`）、`system.rs`↔`SystemTool`（原 `SystemInfoTool`）。新代码避免再制造 `Xxx` 与文件名不同词的情况。

---

## 5. 命名自查清单（提交前）

- [ ] Rust 文件 / 模块 snake_case，类型 PascalCase，常量 UPPER_SNAKE
- [ ] 缩写整词统一（`stt`/`ocr`/`tts`），不混用别名
- [ ] 实体 ID 用 `{prefix}-{uuid32}`，经 `new_id` 生成
- [ ] Svelte 组件文件名 = 组件名（PascalCase）；已迁移组件脚本使用 TypeScript，模块 camelCase，store 尾缀 `Store`
- [ ] TypeScript 局部变量 camelCase，常量 UPPER_SNAKE
- [ ] 跨层只在边界转换 snake↔camel
- [ ] 会话恢复用语统一 `resume`，不用 `review`
- [ ] 运行态会话集合与持久会话历史使用不同名称；全量删除与运行态清理动词明确
- [ ] 工具调用、会话、ToolRun、后台工具运行、定时工具运行按本节口径使用
- [ ] 集合用复数、单元素用单数、派生集合不用裸形容词（`selected`→`selectedSessions`）
- [ ] 文件名与结构体/实体单复数一致（`file.rs`→`files.rs` 对应 `FilesTool`；仓库随表复数）
