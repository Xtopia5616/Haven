# 发布与数据重置

配置运行时 apply 失败时，已通过同目录临时文件和路径替换写入的配置会保留，Settings 会说明部分运行时未应用；用户重启应用后，运行时从配置重新初始化。保存成功不承诺操作系统崩溃或突然断电后的稳定介质持久性。此 apply-failure 语义本身不更改 `config.toml` 格式或数据库 schema；本版本授权变更的 schema 重置要求见下文。

## 当前版本的兼容性政策

Haven 处于测试阶段。数据库 schema、`config.toml`、ReAct snapshot 与内部 IPC 契约可以进行破坏性调整；发布说明会明确本次是否需要重置。没有明确写出兼容承诺的旧数据不得假定可继续使用。

截至 2026-10-03，当前数据库契约为 schema v33（ADR 0392、0393、0402、0416、0445）：scheduled dependency relation/result 持久化在 `actions.watch_action_id` / `actions.result_summary`，scheduled tool 的 completed/failed result 使用 `action_completion_outbox`，session authorization grants 由会话外键级联管理；`pending_session_inputs` 持久跟踪已接受但尚未进入 `UserInject` event 的用户输入，不再使用两天恢复窗口。后台 Shell Action 通过 `actions.source_step_id` 持久关联产生它的 Agent 工具步骤，并在 Action event 与终态结果交付中保留。v33 不做旧 schema 运行时迁移；升级前，完全退出 Haven 后删除 `%APPDATA%\haven\haven.db`、`haven.db-wal` 与 `haven.db-shm`（非 Windows 开发环境为 `~/.local/share/haven` 下的同名文件），再启动应用。删除数据库会清除会话、记忆、任务和用量；保留 `config.toml` 时无需删除整个数据根目录。

## 当前配置契约

`config.toml` 只接受当前配置结构，不执行旧字段搬迁、旧名称映射、凭据导入或静默兼容。配置表启用未知字段拒绝；当前结构允许缺省的字段仍使用安全默认值。旧字段（例如 `[memory].history_retention_days`、`llm.balanced_model`、旧安全策略字段和已删除的顶层 `[audio]`）会导致整份配置解析失败。

解析失败时，Haven 将原文件复制到带时间戳的 `config.toml.*.bak`，并在当前进程使用默认配置；原文件不会在启动时自动转换或覆盖。需要继续使用时，按下文“仅重建配置”删除当前 `config.toml`，再在应用中重新配置。

当前会话保留期配置位于 `[session].history_retention_days`，默认 90 天，设为 `0` 可禁用自动删除；旧 `[memory]` 位置不迁移。`security.permission_mode` 支持 `plan`、`default`、`auto_edit`、`autonomous`，文件沙箱和网络策略分别使用 `sandbox_mode` 与 `network_policy`；旧确认模式字段不再解释。权限 key 使用点号 operation 名（如 `files.read`）；旧冒号 key 不转换，无法解析的持久权限规则在运行时被忽略并 fail-closed。

API 密钥、OCR 密钥与 MCP 环境变量只通过安全凭据存储的 opaque reference 持久化。TOML 中的明文凭据会使配置加载失败并备份原文件；启动只从现有引用读取凭据，不再导入旧明文值。若引用在操作系统凭据存储中不存在，需要重新输入对应凭据。当前 provider `api_style` 仅接受 canonical wire protocol id；无效值和指向不存在 provider 的媒体配置会走同一备份与默认配置恢复。

模型工具使用点号 operation view，例如 `files.*`、`system.*`、`process.*`、`clipboard.*`、`input.*`、`window.*`、`media.*`、`actions.*`、`schedule.*`、`preferences.*`、`checklist.*` 和 `haven.*`。启用 Skill 由 `load_skill` 按名称加载为当前 session 的 `skill__...`；内置 operation 由 `tool_catalog` 的 `action=load` 加载，MCP 由 `load_mcp` 按服务器加载。配置和未完成会话都没有旧工具名的转换保证。

本次 Agent 版本将数据库 schema 收敛为 v28 当前契约：删除 `sessions.transcript` 快照列、`react_checkpoints` 和 `sessions.react_state`，
会话正文只从 `session_events` 恢复并由 `messages` 物化；新增 `session_events` append-only
会话事件表（`sequence`、`event_type`、`event_version`、JSON payload、run/step identity）和
事件流本身承载恢复边界；运行态不再写入大型 JSON snapshot，也没有独立的
checkpoint 表；消息新增 `media_inputs` canonical
媒体表示列。旧数据库不再执行运行时 schema/data 迁移，也不会尝试拼接旧表、旧列或旧
FTS/embedding 形状；消息表的旧 `attachments` 列已删除并由仅供 UI/资产保留使用的
`ui_metadata` 取代，`media_inputs` 是唯一 canonical 媒体持久化来源；`llm_usage.call_kind` 将 Agent 主循环和工具拥有的媒体推理调用分开，
后者保留明细但不进入 `session_usage` 的 Agent 累计 token/费用/缓存率；其它工具内部 LLM 调用使用
`call_kind=tool`，同样只保留明细。`user_version` 不是 v28 的数据库，
或没有版本戳但已经包含用户表，都会拒绝打开；必须删除 `haven.db`、`haven.db-wal` 和
`haven.db-shm` 后重新创建。这样会同时清除会话、记忆、任务、快照和用量；若配置仍需保留，
只删除这三个数据库文件即可，不必删除整个数据根目录。当前契约还包含
`action_completion_outbox`，用于在 broadcast 丢失、进程重启或会话终态清理竞态后
reconcile 后台任务结果。

本版本同时将 session 与 action 生命周期收敛为 typed 状态契约：session 只允许
`pending`、`running`、`paused`、`completed`、`error`，后台/定时任务只允许
`waiting`、`running`、`completed`、`failed`、`cancelled`。定时任务不再使用
`scheduled` 作为状态，也不再用 `actions.fired` 布尔列表达终态；`kind=scheduled`
只表示任务类型，取消或触发后保留为终态历史。旧数据库必须按本节删除并重建。

本版本同时删除了旧的 ask/confirm 等待字段和 session 状态，统一使用
`InteractionRequest` 及其 session domain events。`sessions.react_state` 已从 schema v28
删除，不做运行时迁移，也不再作为测试列保留；含有该列或旧 snapshot 的数据库必须按本节删除后重新创建。

本版本将事实图谱的物理表从 `memory_edges` 统一为 `facts`，并删除 Agent 的
`InferenceEngine` 兼容入口；当前后台事实编排只使用 `MemoryWorker`。数据库 schema
版本升至 v28，不执行表名迁移。升级前必须删除 `haven.db`、`haven.db-wal` 和
`haven.db-shm` 后重新创建；源代码调用方需直接迁移到当前名称。

本版本的模型工具媒体契约也已收敛：原独立 `audio` 工具已删除，录音、播放、播报、音量和静音
统一为 `media.record`、`media.play`、`media.speak`、`media.volume_*` 和 `media.mute_*`；
`media.*` 的内容派生仍使用 `asset_id`，`window.screenshot` / `window.ocr` 不再接受宿主
`path`；窗口截图会登记到生成媒体目录，图片、音频和支持的文档通过对应的 `media.*` view 派生。
旧 `audio:*` 权限不会自动映射到 `media.*`；含旧 audio/window path 调用的未完成 ReAct snapshot 不保证恢复；请删除
数据库与媒体缓存后重新开始会话，不要混用新旧运行态数据。

本版本进一步收敛模型工具入口：`system.*`、`process.*`、`clipboard.*`、`input.*`、
`window.*`、`media.*` 和 `haven.*` 均使用独立点号 view。风险不会按根工具统一计算，
每个 view 仍独立执行 schema、授权、确认和并发策略；例如 `process.kill` 与 `system.info`、
`haven.mcp.mcp_add` 与 `haven.mcp.mcp_list` 的风险分别保持 High/Safe、High/Low。旧根名的
未完成 ReAct snapshot 不保证恢复。

## 用户数据位置

Windows 的唯一数据根目录是 `%APPDATA%\haven`。其中包括：

- `config.toml`：模型、工具与安全确认配置；
- `haven.db`（及 SQLite journal/WAL）：会话、记忆与任务；
- `logs/`、`media/`、`skills/` 与 `inbox/`：日志、生成媒体、本地技能及多会话消息。

非 Windows 开发环境使用 `~/.local/share/haven`。路径由 `haven_common::config::ConfigLoader::data_dir()` 统一决定，宿主层不得另行推导。

## 重置步骤

### 仅重建配置

1. 完全退出 Haven，并确认没有 `Haven.exe` 进程仍在运行。
2. 如需检查旧配置，先在 `%APPDATA%\haven` 之外复制 `config.toml`；旧文件或自动备份可能包含明文密钥，必须妥善保管。
3. 删除 `%APPDATA%\haven\config.toml`（非 Windows 开发环境为 `~/.local/share/haven/config.toml`）。
4. 重新启动 Haven，再配置模型、OCR 与 MCP 凭据。数据库、日志、技能和媒体目录会保留。

### 完整重置数据根目录

1. 完全退出 Haven，并确认没有 `Haven.exe` 进程仍在运行。
2. 如需保留配置或诊断资料，先在数据根目录之外复制所需文件；备份内容可能含密钥、对话和本机路径，必须妥善保管。
3. 删除 `%APPDATA%\haven`（非 Windows 为 `~/.local/share/haven`）。
4. 重新启动 Haven；应用会创建新的默认配置和数据库。

完整重置会永久删除本机会话、记忆、后台/定时任务、授权决定、日志、技能和媒体缓存；除非先自行备份，否则无法恢复。若仅需要清除会话与记忆，可删除 `haven.db`、`haven.db-wal`、`haven.db-shm`，但在 schema 不兼容的版本升级时应删除这三个数据库文件。配置不兼容时只需按上面的步骤删除 `config.toml`。

## 发布前验证

发布候选版本必须在全新数据根目录完成：启动、默认配置创建、模型配置、会话、工具确认、媒体和任务流程、重启恢复、升级重置和卸载验证。执行的自动化质量门禁见仓库根目录 [README](../README.md)；它们不能替代真实桌面安装流程。

自动化测试必须使用专用、运行前确认不存在的数据根目录。Rust workspace 测试应把 `APPDATA` 指向仓库 `target` 下唯一的审计/测试目录，避免默认 `%APPDATA%\haven`；会启动清理任务的 AppState 测试必须注入 fixture 自己的上传和生成媒体根目录。不要把测试 `APPDATA` 设置为日常使用的配置路径，也不要用现有用户目录验证重置或卸载。

首次启动、安装升级、数据库重置和卸载需在一次性 Windows 用户配置或 VM 中验证，并在操作前确认目标路径属于该临时环境。仓库当前的单元/集成测试不执行这些破坏性桌面步骤。2026-09-26 最终架构审计的隔离方式、通过门禁和未执行项见 [ADR 0361](adr/0361-final-architecture-acceptance-audit.md)。

## 回滚

仅在保留了升级前的完整数据根目录备份，并且旧二进制与该数据版本兼容时，才可通过恢复备份回滚。没有兼容性保证时，回滚方案是安装目标版本后按上述步骤重置数据，而不是混用新旧数据库或快照。
