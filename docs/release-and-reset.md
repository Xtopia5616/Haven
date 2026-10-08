# 发布与数据重置

配置运行时 apply 失败时，已通过同目录临时文件和路径替换写入的配置会保留，Settings 会说明部分运行时未应用；用户重启应用后，运行时从配置重新初始化。保存成功不承诺操作系统崩溃或突然断电后的稳定介质持久性。此 apply-failure 语义本身不更改 `config.toml` 格式或数据库 schema；本版本授权变更的 schema 重置要求见下文。

## 当前版本的兼容性政策

Haven 处于测试阶段。数据库 schema、`config.toml` 与内部 IPC 契约可以进行破坏性调整；发布说明会明确本次是否需要重置。没有明确写出兼容承诺的旧数据不得假定可继续使用。

截至 2026-10-08，当前数据库契约为 schema v39（ADR 0392、0393、0402、0416、0442、0445、0458、0463、0464、0524、0633、0809）：后台与定时工具运行统一持久化在 `tool_runs`，依赖关系使用 `watch_tool_run_id` / `result_summary`，定时运行完成结果使用 `tool_run_completion_outbox`；session authorization grants 由会话外键级联管理。`pending_session_inputs` 持久跟踪尚未进入 `UserInject` event 的用户输入，并在同一事务保存 `answer` / `follow_up` disposition，不再使用两天恢复窗口。唯一的 pending Answer reservation 防止 Ask 尚未由 `UserInject` 确认时后续输入被重复路由为答案；恢复只读取该 marker，不按重启时的交互 gate 重新判断。`sessions.origin` 与 `sessions.parent_session_id` 持久记录普通用户会话或 `agent.spawn` peer 的来源及 parent lineage，且不改变 session lifecycle。sessions.run_end_reason 保存最近一次 paused、completed 或 error 运行的净化后原因；新一轮进入 pending/running 时清除。后台 shell ToolRun 持久化 `tool_runs.source_step_id`，关联发起它的 Agent `ToolCall`，并在 ToolRun event 与终态结果交付中保留。v37 不再读取旧 interaction event 的 `prompt` 字段或缺少 payload `step_number` 的旧 compaction event，也不做旧 schema 运行时迁移；升级前，完全退出 Haven 后删除 `%APPDATA%\haven\haven.db`、`haven.db-wal` 与 `haven.db-shm`（非 Windows 开发环境为 `~/.local/share/haven` 下的同名文件），再启动应用。旧的 `actions` 表、`act-` ID、completion outbox 与 tool name 不做兼容迁移。删除数据库会清除会话、记忆、工具运行和用量。

schema v39 仅接受事实提取 marker 的当前完整格式：旧 boolean、缺 retry metadata 的短格式和 summary value-only marker 都不再解析；`CacheDiagnostics` 也要求当前完整 metadata shape。升级前必须按下文删除数据库。`MediaProbe` 使用 `media_kind` / `mime_type` Serde key，不接受旧 `media_type` key；它不属于持久表或当前 IPC DTO。MCP `inputSchema` 和 LLM provider wire 字段仍按当前外部协议解析，不属于 Haven 历史数据兼容。

## 当前配置契约

`config.toml` 只接受当前配置结构，不执行旧字段搬迁、旧名称映射、凭据导入或静默兼容。配置表启用未知字段拒绝；当前结构允许缺省的字段仍使用安全默认值。本版本将 `context_limits.cut_off_retries` 改为 `incomplete_tool_args_retries`，将后台/终态 Job 限制字段重命名为 ToolRun 字段，并删除 `empty_response_max_retries` 与 `empty_response_retry_delay_ms`；含这些旧字段的配置会导致整份配置解析失败。工具根名从 `actions` 改为 `tool_runs`，旧名称下的权限 key 不迁移。`llm.models[]` 中绑定连接的字段现为 `provider_name`；原 `provider` 字段不再接受，含旧字段的配置会导致整份配置解析失败。Session 步数预算配置现为 `[session].max_steps_per_run` 与 `[session].max_steps_per_session`；旧 `max_steps` / `session_max_steps` 不接受，遇到旧 key 时整份配置解析失败；如需保留预算数值，先手动将两项改为新 key，否则重建配置。其它旧字段（例如 `[memory].history_retention_days`、`llm.balanced_model`、旧安全策略字段和已删除的顶层 `[audio]`）也会导致整份配置解析失败。

Session prompt 首次上下文条数目前配置在 `[session].prompt_history_limit`（默认 50）；旧 `[memory].session_window_size` 不再接受或自动搬迁。若要保留其它配置，手动将数值移到新 key 并删除旧 key；否则按“仅重建配置”删除 `config.toml` 后重新设置。遇到旧 key 时配置会整体解析失败、先备份原文件并在当前进程使用默认值；数据库不受影响。

解析失败时，Haven 将原文件复制到带时间戳的 `config.toml.*.bak`，并在当前进程使用默认配置；原文件不会在启动时自动转换或覆盖。需要继续使用时，按下文“仅重建配置”删除当前 `config.toml`，再在应用中重新配置。

当前会话保留期配置位于 `[session].history_retention_days`，默认 90 天，设为 `0` 可禁用自动删除；旧 `[memory]` 位置不迁移。`security.permission_mode` 支持 `plan`、`default`、`auto_edit`、`autonomous`，文件沙箱和网络策略分别使用 `sandbox_mode` 与 `network_policy`；旧确认模式字段不再解释。权限 key 使用点号 operation 名（如 `files.read`）；旧冒号 key 不转换，无法解析的持久权限规则在运行时被忽略并 fail-closed。

API 密钥、OCR 密钥与 MCP 环境变量只通过安全凭据存储的 opaque reference 持久化。TOML 中的明文凭据会使配置加载失败并备份原文件；启动只从现有引用读取凭据，不再导入旧明文值。若引用在操作系统凭据存储中不存在，需要重新输入对应凭据。当前 provider `api_style` 仅接受 canonical wire protocol id；无效值和指向不存在 provider 的媒体配置会走同一备份与默认配置恢复。Deepgram 凭据只填写原始 API key，不要包含 `Token ` 或 `Deepgram ` 前缀；已保存前缀的凭据需要在模型设置中重新输入。

模型工具使用点号 operation view，例如 `files.*`、`system.*`、`clipboard.*`、`input.*`、`window.*`、`media.*`、`tool_runs.*`、`schedule.*` 和 `haven.*`。启用 Skill 由 `load_skill` 按名称加载为当前 session 的 `skill__...`；内置 operation 由 `tool_catalog` 的 `action=load` 加载，MCP 由 `load_mcp` 按服务器加载。配置和未完成会话都没有旧工具名的转换保证。

Skill 名称现在统一限制为 1–128 个 ASCII 字母、数字、`-` 或 `_`，并拒绝 Windows 设备保留名（`CON`、`PRN`、`AUX`、`NUL`、`COM1`–`COM9`、`LPT1`–`LPT9`，不区分大小写）。扫描时，同名大小写变体会作为歧义组全部跳过。已有 `SKILL.md` 使用不符合规则的名称时，Skill 会被跳过；将清单中的名称改为有效名称即可恢复使用。若配置了 `[skills].enabled` allowlist，也要同步更新对应条目。此名称规则不会改变数据库契约，改名无需重置数据库。

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
4. 重新启动 Haven，再配置模型、OCR 与 MCP 凭据。schema v39 版本升级还需按下方说明删除数据库。

### 完整重置数据根目录

1. 完全退出 Haven，并确认没有 `Haven.exe` 进程仍在运行。
2. 如需保留配置或诊断资料，先在数据根目录之外复制所需文件；备份内容可能含密钥、对话和本机路径，必须妥善保管。
3. 删除 `%APPDATA%\haven`（非 Windows 为 `~/.local/share/haven`）。
4. 重新启动 Haven；应用会创建新的默认配置和数据库。

完整重置会永久删除本机会话、记忆、后台/定时 ToolRun、授权决定、日志、技能和媒体缓存；除非先自行备份，否则无法恢复。schema v39 升级要求删除 `haven.db`、`haven.db-wal` 与 `haven.db-shm`，否则旧数据库会被拒绝打开。只有配置仍含不再接受的旧 key（如 `llm.models[].provider`）时，才另外按“仅重建配置”删除 `config.toml`。

## 发布前验证

发布候选版本必须在全新数据根目录完成：启动、默认配置创建、模型配置、会话、工具确认、媒体和任务流程、重启恢复、升级重置和卸载验证。执行的自动化质量门禁见仓库根目录 [README](../README.md)；它们不能替代真实桌面安装流程。

自动化测试必须使用专用、运行前确认不存在的数据根目录。Rust workspace 测试应把 `APPDATA` 指向仓库 `target` 下唯一的审计/测试目录，避免默认 `%APPDATA%\haven`；会启动清理任务的 AppState 测试必须注入 fixture 自己的上传和生成媒体根目录。不要把测试 `APPDATA` 设置为日常使用的配置路径，也不要用现有用户目录验证重置或卸载。

首次启动、安装升级、数据库重置和卸载需在一次性 Windows 用户配置或 VM 中验证，并在操作前确认目标路径属于该临时环境。仓库当前的单元/集成测试不执行这些破坏性桌面步骤。2026-09-26 最终架构审计的隔离方式、通过门禁和未执行项见 [ADR 0361](adr/0361-final-architecture-acceptance-audit.md)。

## 回滚

仅在保留了升级前的完整数据根目录备份，并且旧二进制与该数据版本兼容时，才可通过恢复备份回滚。没有兼容性保证时，回滚方案是安装目标版本后按上述步骤重置数据，而不是混用新旧数据库或快照。
