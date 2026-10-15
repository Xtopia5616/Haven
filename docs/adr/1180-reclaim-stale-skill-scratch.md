# ADR 1180：有界回收崩溃遗留的 Skill scratch

## 状态

已采纳并实施。

## 背景

ADR 1179 为每次 Skill 调用分配独立随机 scratch 目录，正常完成、失败、超时和取消时由 RAII 删除。Haven 被强制结束时 Rust 析构不会执行，配置 scratch 根可能积累 `haven-skill-run-*` 目录。对整个根目录做递归删除或无界枚举可能误删无关内容并造成启动或执行阻塞。

## 决定

- 每个 scratch 目录创建 `.haven-owner.lock`，SkillRunner 持有独占 OS 文件锁直到正常清理开始。锁不授予脚本访问权，也不替代进程 Job Object。
- 创建 scratch root 前先尝试通过 capability 打开现有路径；只有 `NotFound` 才创建缺失路径，现存的重解析祖先会在创建前 fail closed。
- 每次创建新 scratch 前，通过已经打开的父目录 capability 枚举直接子项。一次最多检查 4096 项，超过上限则本轮 fail closed；只考虑 `haven-skill-run-` 加 6–32 位 ASCII 字母数字后缀的普通目录。
- 只回收超过 `MAX_SKILLS_EXEC_TIMEOUT_SECS + 60s` 的目录，每轮最多删除 16 个。锁文件存在时必须能够以读写句柄取得独占锁；锁被占用、条目含 reparse point、文件信息不完整、名称不匹配或无法确认时保留。没有锁文件的旧版孤儿仅按目录修改时间判定，供回收 ADR 1179 初版留下的崩溃残留。
- 删除始终相对于 capability 根执行；递归清理不跟随嵌套 symlink。单个过期目录删除失败时记录脱敏告警并继续 Skill 执行；根目录超过扫描上限时本次执行 fail closed。清理机会由后续 Skill 调用触发，不在应用启动时扫描整个 Temp。

## 替代方案

- 清空 scratch 根或递归扫描整棵 Temp：拒绝，范围超过 Skill 自有随机目录，且可能无界耗时。
- 只按年龄删除：拒绝，调用可能存活至配置的最长执行期限，暂停进程也不能被年龄误判。
- 仅依赖 Job Object：拒绝，Job Object 管理子进程生命周期，不负责回收父进程崩溃前已经创建的文件系统对象。
- 对每个 Skill 提供用户可调清理容量：拒绝，当前没有更高层资源预算契约；固定扫描和删除上限足以限制单轮开销。

## 影响与迁移

过期崩溃残留在下一次 Skill 执行前机会性回收；如果没有后续 Skill 调用、目录仍被占用、超过扫描上限或文件系统拒绝删除，它会继续保留并可能需要用户手动清理。旧版未带 owner 文件的 `haven-skill-run-*` 目录按同一 TTL 回收。无配置、数据库或 Skill 清单迁移，无需删除用户数据。此锁用于并发生命周期判定，不限制 Skill 对其它文件和网络的访问。

## 验证

- Platform 能力枚举测试验证直接子项类型与条目数上限；递归删除测试验证 nested symlink 不会触碰目标外内容。
- Tools 测试验证只删除 stale unlocked 目录，保留活动锁、未过期目录、错误名称、reparse point 和外部目标；跨进程测试验证 owner 进程存活时不回收，退出释放 OS 锁后可以回收。
- 运行格式检查、Tools 与 Platform crate 测试及严格 Clippy；该文件系统生命周期变化在 Windows 环境验证。
