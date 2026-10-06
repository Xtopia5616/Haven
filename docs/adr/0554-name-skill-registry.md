# ADR 0554：Skills 发现状态命名为 SkillRegistry

## 状态

已采纳并实施；Skills crate API 的消费者已经统一迁移。

## 背景

`SkillsEngine` 的实现注释把它定义为 discovered Skills 的 registry。它按 skill name 持有发现结果、enablement allowlist 与 catalog version，并提供刷新、列举和按名查询；它不执行算法循环，执行仍由 Tools 的 Skill adapter/runner 负责。名字中的 `Engine` 因此把注册目录与执行职责混在了一起。

同一个对象的 `list()` / `get()` 返回面向 UI 的 `SkillInfo`，而 `list_skills()` / `get_skill()` 返回运行时 `Skill`，泛化动词使两种表示难以从调用点区分。`enabled_filter()` 没说明过滤的是哪些实体。目录 watcher 还跨 Skills→Tools crate 返回 `(PathBuf, SystemTime, u64)`，调用方只能靠 tuple 位置理解文件路径、修改时间和长度。

## 决定

1. 将 `SkillsEngine` 和内部 `skills_engine` 字段改为 `SkillRegistry` / `skill_registry`。不保留旧 API 别名；当前消费者都在 Haven workspace 内。
2. 将 UI snapshot 查询命名为 `list_skill_infos()` / `get_skill_info()`；保留运行时对象查询 `get_skill()` / `list_skills()`，明确区分 snapshot 与可供执行层使用的实体。
3. 将配置持久化读取方法 `enabled_filter()` 改为 `enabled_skill_allowlist()`。
4. 将 watcher API `folder_signature()` 改为 `list_skill_file_fingerprints()`，返回 `Vec<SkillFileFingerprint>`；字段为 `path`、`modified_at`、`byte_length`。
5. 同步 Rust 调用点、crate re-export、命名规范、跨 crate 输出清单和路线图。Skill 扫描、筛选、排序、版本递增、watcher 触发、Tool 执行、配置持久化以及 Tauri/Tool 输出 shape 均保持不变。

## 替代方案

- 保留 `SkillsEngine`：拒绝。该类型是 keyed discovery registry，不是本规范定义的算法执行引擎。
- 改名为 `SkillCatalog`：拒绝。它不只是只读的发现投影，还拥有可变 enablement、refresh 和 catalog version 状态；`SkillRegistry` 更准确表达其权威内存条目 owner。
- 只改 struct 名，保留 `list/get`：拒绝。同一 owner 同时返回 UI snapshot 和运行时 `Skill`，调用点需要名称体现所选表示。
- 让 watcher 继续返回 tuple：拒绝。该结果跨 crate 携带固定语义的多个字段，应使用具名结构体。

## 影响与验证

- 这是 Skills/Tools/Agent/App 之间的 Rust API 重命名与返回类型具名化，不改变 Tauri 命令、事件 payload、配置 key、数据库或持久数据，无需重置数据。
- 需验证 Rust workspace fmt、check、严格 Clippy 与 tests；并确认 `SkillsEngine`、旧字段/方法和位置 tuple 不再出现在活动代码中。历史 ADR 保留当时名称作为决策记录。

## 回滚

恢复 `SkillsEngine`、旧字段/查询方法和 tuple watcher API，并同步回退所有 Rust 调用点及当前架构/命名清单；无需迁移或重置数据。
