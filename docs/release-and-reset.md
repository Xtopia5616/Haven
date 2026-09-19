# 发布与数据重置

## 当前版本的兼容性政策

Haven 处于测试阶段。数据库 schema、`config.toml`、ReAct snapshot 与内部 IPC 契约可以进行破坏性调整；发布说明会明确本次是否需要重置。没有明确写出兼容承诺的旧数据不得假定可继续使用。

本版本将安全策略重构为互相独立的确认、文件沙箱和网络策略；`security.permission_mode` 有效值为
`plan`、`default`、`auto_edit`、`autonomous`，另有 `sandbox_mode = "read_only" | "workspace_write" | "full_access"`、
可选的绝对路径数组 `writable_roots` 和 `network_policy = "deny" | "restricted" | "open"`。旧的
`balanced`、`careful`、`manual` 以及
`confirmation_mode` / `min_risk_level` 组合不再自动解释，`[security]` 中的未知字段会使配置解析失败；
原配置会被备份为 `config.toml.*.bak` 并以默认配置启动。请按下文完整重置或仅手工重建新的 `[security]` 段。

本版本同样不再迁移顶层 `[audio]`、旧的 `[tool_settings.audio]` 或已删除的 `[tool_settings.*]` 名称；这几类配置会备份后以默认值启动。
旧工具名称不再迁移或兼容：`[tool_settings.file]`、`file[:operation]`、
`file_search[:operation]`、`scheduled_action[:operation]`、旧的聚合根权限和旧的 `haven_*`
capability 入口会触发备份并以默认配置启动。当前模型入口统一为点号 operation view：
`files.*`、`system.*`、`process.*`、`clipboard.*`、`input.*`、`window.*`、`media.*`、
`actions.*`、`schedule.*`、`preferences.*`、`checklist.*` 和 `haven.*`；这些名称同时作为
权限 key 与 UI renderer 的正式名称。聚合实现仍可供 native/Tauri 使用，但不再作为模型入口。
启用 Skill 只出现在紧凑能力索引中，由模型调用 `load_skill` 按名称加载为当前 session 的
`skill__...`；内置 operation 可由 `load_builtin` 按 operation/root 加载，MCP 继续使用
`load_mcp` 按服务器加载。完整工具 schema 只在加载成功后的后续 provider 请求中出现。
已删除的 `haven_session_diagnostics` 及其 operation 权限也不再迁移；升级时会触发同样的备份与配置重置。
Provider 的 `api_style` 现在只接受 canonical wire protocol id；旧的 vendor/preset 值
（例如 `deepseek-responses`）不会再作为 wire style 解释，检测到后同样备份并重置配置。
数据库中待执行定时任务若仍引用已经删除的旧工具名不会自动改写，需取消并重新创建，或按下文完整重置。

旧 Phase-7 ReAct 快照（包括未压缩的旧 `react_state` 行）以及直接在 `[media.stt]`、`[media.tts]`、`[media.image_gen]` 中使用
旧 provider 名和本地凭据的配置也不再兼容。加载器会为检测到的旧媒体 provider 名或凭据生成
`config.toml.*.bak`，并以默认值启动；请删除整个数据根目录后重新配置命名 provider，不要
手工混用新数据库与旧 `react_state`。

本版本删除 `balanced_model` 角色及默认模型失败后的模型级 fallback；旧配置中的该角色和
`fallback_retry_max_retries` 会被忽略，保存配置后不再写回。若需要清理旧配置残留，按下文
完整重置数据根目录后重新配置模型。

本次 Agent 版本将数据库 schema 收敛为 v22 当前契约：新增 `session_events` append-only
会话事件表（`sequence`、`event_type`、`event_version`、JSON payload、run/step identity）和
checkpoint 的 `event_sequence` 高水位；消息新增 `media_inputs` canonical
媒体表示列。旧数据库不再执行运行时 schema/data 迁移，也不会尝试拼接旧表、旧列或旧
FTS/embedding 形状；`llm_usage.call_kind` 将 Agent 主循环和工具拥有的媒体推理调用分开，
后者保留明细但不进入 `session_usage` 的 Agent 累计 token/费用/缓存率；其它工具内部 LLM 调用使用
`call_kind=tool`，同样只保留明细。`user_version` 不是 v22 的数据库，
或没有版本戳但已经包含用户表，都会拒绝打开；必须删除 `haven.db`、`haven.db-wal` 和
`haven.db-shm` 后重新创建。这样会同时清除会话、记忆、任务、快照和用量；若配置仍需保留，
只删除这三个数据库文件即可，不必删除整个数据根目录。

本版本同时将 session 与 action 生命周期收敛为 typed 状态契约：session 只允许
`pending`、`running`、`paused`、`completed`、`error`，后台/定时任务只允许
`waiting`、`running`、`completed`、`failed`、`cancelled`。定时任务不再使用
`scheduled` 作为状态，也不再用 `actions.fired` 布尔列表达终态；`kind=scheduled`
只表示任务类型，取消或触发后保留为终态历史。旧数据库必须按本节删除并重建。

本版本同时删除了旧的 ask/confirm 等待字段和 session 状态，统一使用
`InteractionRequest`（快照字段 `interactions`）。旧 `react_state` 不做运行时迁移；若打开旧快照
失败，请按本节删除数据库文件后重新创建。

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

1. 完全退出 Haven，并确认没有 `Haven.exe` 进程仍在运行。
2. 如需保留配置或诊断资料，先在数据根目录之外复制所需文件；备份内容可能含密钥、对话和本机路径，必须妥善保管。
3. 删除 `%APPDATA%\haven`（非 Windows 为 `~/.local/share/haven`）。
4. 重新启动 Haven；应用会创建新的默认配置和数据库。

此操作会永久删除本机会话、记忆、后台/定时任务、授权决定、日志、技能和媒体缓存；除非先自行备份，否则无法恢复。若仅需要清除会话与记忆，可删除 `haven.db`、`haven.db-wal`、`haven.db-shm`，但在 schema 或配置不兼容的版本升级时应删除整个数据根目录。

## 发布前验证

发布候选版本必须在全新数据根目录完成：启动、默认配置创建、模型配置、会话、工具确认、重启恢复、数据重置和卸载验证。执行的质量门禁见仓库根目录 [README](../README.md)。

## 回滚

仅在保留了升级前的完整数据根目录备份，并且旧二进制与该数据版本兼容时，才可通过恢复备份回滚。没有兼容性保证时，回滚方案是安装目标版本后按上述步骤重置数据，而不是混用新旧数据库或快照。
