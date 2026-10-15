# ADR 1214：通过固定目录 capability 创建 Skill 文件树

## 状态

已实现。Skill 创建与失败清理使用固定 Skills 根目录 capability；清单文件最后原子发布。

## 背景

`skill_create` 先创建目录，再通过拼接后的路径分别写入 `SKILL.md` 与 `scripts/main.py`。这种写法没有固定目录句柄，也会跟随最终文件路径；如果写入目标是硬链接，截断写会修改硬链接的另一名称。逐个写入清单和脚本还可能让并发目录扫描看见不完整的 Skill。

## 决定

- 创建前解析并固定 Skills 根目录，后续创建、原子写入和失败清理由同一个 `DirectoryCapability` 完成。
- 先以 capability 原子创建 Skill 目录，再写 `scripts/main.py`，最后原子替换 `SKILL.md`。清单最后发布，使目录扫描不能加载缺少脚本的半成品。
- `atomic_replace` 替换目录项而不是跟随目标文件；已有非普通文件和重解析点按 capability 规则拒绝。
- 文件写入或注册表/配置更新失败时，通过相同 capability 清理新目录，并恢复 Skill 注册表的原 enabled allowlist。清理失败只记录脱敏诊断，不改变原始错误。
- Skills 根目录创建仍遵循现有配置；此 ADR 不改变执行工作区、沙箱权限、配置格式或用户授权语义。

## 替代方案

- 保留 `tokio::fs::write` 并在调用前做路径检查：拒绝；检查与路径打开之间仍有竞态，且写硬链接会修改共享文件内容。
- 逐文件创建并让扫描器忽略不完整项目：拒绝；这会把半成品过滤逻辑扩散到 Skills reader，而清单最后发布能维持单一加载边界。
- 对失败文件逐路径删除：拒绝；清理必须继续锚定已验证的 Skills 根目录。

## 影响与迁移

Skill 创建现在通过平台目录 capability 的同步文件操作执行，并用 `spawn_blocking` 避免阻塞异步 executor。新增目录树写入 helper、失败回滚和硬链接回归测试。无数据库、配置、IPC 或 Skills manifest schema 变化，无需迁移或重置。

## 验证

- 硬链接回归测试验证替换 `SKILL.md` 后，另一个硬链接名称仍保留原内容。
- `haven-tools` locked 测试、严格 Clippy 与格式检查覆盖本切片；Windows 文件系统集成结果以 Windows CI 为准。
- 本 ADR 的索引、路线图和 staged diff 经检查。

## 回滚

回滚时恢复原来的路径写入实现并删除硬链接回归测试与本 ADR/路线图说明。无持久数据需要恢复；但旧写路径会重新引入对路径重解析及别名文件的依赖。
