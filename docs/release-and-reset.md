# 发布与数据重置

## 当前版本的兼容性政策

Haven 处于测试阶段。数据库 schema、`config.toml`、ReAct snapshot 与内部 IPC 契约可以进行破坏性调整；发布说明会明确本次是否需要重置。没有明确写出兼容承诺的旧数据不得假定可继续使用。

本版本删除了安全确认模式 `confirmation_mode = "always"` 的兼容别名；有效值仅为
`ask`、`paranoid`、`autopilot`。旧配置会被备份为 `config.toml.*.bak` 并以安全默认值启动；请在
备份中将该字段改为 `ask` 后再手工合并，或按下文完整重置。

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
