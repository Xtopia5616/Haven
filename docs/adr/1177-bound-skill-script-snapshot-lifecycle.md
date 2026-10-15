# ADR 1177：收口 Skill 脚本快照生命周期

## 状态

已采纳并实施。

## 背景

执行前从已校验句柄创建的 Skill 脚本快照由 `TempPath` 在正常返回、失败、超时或取消时删除，单文件上限为 16 MiB。进程崩溃会跳过析构，旧快照此前直接散落在 Haven runtime Temp 根目录，没有定向回收 owner。

## 决定

- 新快照写入 runtime Temp 下独立的 `skill-script-snapshots` 目录。`TempPath` 继续负责单次调用的正常清理。
- `SkillRunner` 构造时清理共享 runtime Temp 根与专用子目录中匹配 `haven-skill-script-<6..32 位 ASCII 字母数字>.py` 的普通文件，仅当修改时间超过最长 Skill 执行期限（3600 秒）再加 60 秒余量时删除。旧版直接写在 runtime Temp 根的崩溃残留也仅按同一规则处理。
- 清理打开目标目录 capability，忽略 symlink/reparse point、目录和不匹配名称；清理失败只记录脱敏告警，不阻止 Skill 使用。该操作不是子进程文件系统沙箱，也不构成并发快照总容量限制。
- 单元测试必须显式注入 `tempdir`；生产构造器仍使用 Haven runtime Temp，不触碰测试用户目录。

## 替代方案

- 扫描或清空整个 Temp 根：拒绝，会删除与 Skill 无关的文件。
- 按较短固定 TTL 删除：拒绝，最长合法执行可运行 3600 秒；清理阈值必须晚于该期限。
- 依赖 TempPath 消除所有崩溃残留：拒绝，进程异常终止时析构不运行。

## 影响与迁移

新快照仅改变 Haven 内部 Temp 目录布局，不改变 Skill 输入、相对文件访问或权限。升级后，超过 3660 秒的旧命名快照会在 runner 构造时删除；更年轻的文件及所有其他 Temp 内容保留。无配置或持久数据变化，无需重置。单次文件仍最多 16 MiB；并发快照聚合容量继续作为后续资源预算复核项。

## 验证

- 隔离临时目录测试确认只删除过期的规范命名普通文件，并保留新文件、格式错误名称、目录和无关文件。
- SkillRunner 超时和输出超限测试确认 `TempPath` 在失败退出后回收快照。
- 运行 workspace 格式检查与 Tools crate 全量测试、严格 Clippy；清理测试不读取或修改真实用户 Temp。
