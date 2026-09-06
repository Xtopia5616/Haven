# 发布与数据重置

## 当前版本的兼容性政策

Haven 处于测试阶段。数据库 schema、`config.toml`、ReAct snapshot 与内部 IPC 契约可以进行破坏性调整；发布说明会明确本次是否需要重置。没有明确写出兼容承诺的旧数据不得假定可继续使用。

本版本删除了安全确认模式 `confirmation_mode = "always"` 的兼容别名；有效值仅为
`ask`、`paranoid`、`autopilot`。旧配置会被备份为 `config.toml.*.bak` 并以安全默认值启动；请在
备份中将该字段改为 `ask` 后再手工合并，或按下文完整重置。

本版本同样不再迁移顶层 `[audio]` 或已删除的 `[tool_settings.*]` 名称；这两类配置会备份后以默认值启动。
仍受支持的历史工具别名只做一次性加载迁移：`[tool_settings.file]` 转为
`[tool_settings.files]`，`scheduled_action[:operation]` 权限键转为
`schedule[:operation]`，若新旧配置同时存在则新名称优先，后续保存不会写回旧名称。
数据库中待执行定时任务若仍引用已经删除的旧工具名不会自动改写，需取消并重新创建，或按下文完整重置。

旧 Phase-7 ReAct 快照（包括未压缩的旧 `react_state` 行）以及直接在 `[media.stt]`、`[media.tts]`、`[media.image_gen]` 中使用
旧 provider 名和本地凭据的配置也不再兼容。加载器会为检测到的旧媒体 provider 名或凭据生成
`config.toml.*.bak`，并以默认值启动；请删除整个数据根目录后重新配置命名 provider，不要
手工混用新数据库与旧 `react_state`。

本版本删除 `balanced_model` 角色及默认模型失败后的模型级 fallback；旧配置中的该角色和
`fallback_retry_max_retries` 会被忽略，保存配置后不再写回。若需要清理旧配置残留，按下文
完整重置数据根目录后重新配置模型。

本次 Agent 版本将数据库 schema 升至 v12，为 `session_steps` 增加工具调用顺序和
`tool_call_id`。已有数据库会由迁移补齐列；没有这两个字段的旧步骤在无快照恢复时只能使用
基于步骤 ID 的确定性 fallback。正在等待确认的旧快照缺少完整调用身份时不应继续混用，建议按
下文完整重置后重新发起工具调用。

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
