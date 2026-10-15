# ADR 1174：限制前台 Shell 失败日志的生命周期

## 状态

已采纳并实施。

## 背景

前台 Shell 命令失败时会把完整原始输出写入 `shell-logs`，并把路径放入工具结果。该文件没有 ToolRun durable row 或单独的删除 owner；旧文件名仅使用秒级时间戳，可能在同一秒的失败调用间覆盖，而且目录没有容量上限。ToolRun 后台日志已有独立的历史引用和删除策略，不能套用前台策略。

## 决策

- 前台 Shell 日志使用 `shell-<uuid32>.log`，避免并发/同秒失败互相覆盖；旧版 `shell-<epoch-seconds>.log` 只由该 owner 识别并按相同过期规则回收。
- 日志为尽力保留的诊断产物：保留最多 7 天、128 个文件及 32 MiB 总量；超限时按修改时间先清最旧文件。单文件最多 1 MiB，超出时保留 UTF-8 安全的尾部并写明截断标记。
- 启动恢复与每次写入前都清理过期/超预算日志和 `DirectoryCapability::atomic_replace` 遗留的 `.file-<uuid32>.tmp` staging。只有符合 Haven 文件名的普通直接子文件可被清理；目录 capability 锚定删除和写入。
- 前台 Shell 清理与写入由 Tools 内进程内互斥串行。写日志或清理失败只记录脱敏诊断，不改变 Shell 命令本身的成功/失败结果。
- `tool-run-logs/<toolrun-id>.log` 继续由 ToolRun durable history 引用、删除和启动孤儿扫描管理，不受该期限与容量策略影响。

## 替代方案

- 让前台日志永久保留：拒绝，会使没有持久 owner 的 Temp 文件持续增长。
- 依据 transcript 中的文本路径清理：拒绝，路径不是稳定授权身份，且不能安全界定删除目标。
- 将两类日志统一按 TTL 清理：拒绝，ToolRun 日志的生命周期受 durable history 引用控制，保留记录仍需能读取完整结果。

## 影响与迁移

日志路径仍只作为本地诊断路径出现在 Shell 工具结果中，不改变 IPC、配置或数据库。旧时间戳日志在过期或预算回收时删除；新策略最多保留 7 天，历史结果中的旧路径可能不再可用。数据库和配置无需重置。

## 验证

- 在隔离临时目录验证旧/新文件名清理、staging 回收、数量预算、原子写入和 UTF-8 字节上限。
- 运行 Tools crate 与 workspace Rust 测试、严格 Clippy 和格式检查；测试不遍历或清理真实用户 Temp。
