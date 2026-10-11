# 0927：由 Skills owner 封装目录变更检测

## 状态

已接受并实现（2026-10-11）。

## 背景

Tools 的 Skills watcher 直接取得 Skills 的文件 fingerprint DTO，并在 Tools 内维护上次观察结果。`SkillRegistry` 同时提供列举文件 fingerprint 与全量刷新两个操作；Tools 因而理解 Skills 内部扫描依据、路径/时间戳结构和失败时保留旧 fingerprint 的重试规则。该 DTO 只为一个 watcher 服务，却成为跨 crate API。

另外，`haven_skills::venv` 实现模块公开可寻址，`VenvManager::root()` 没有消费者；Tools 还在默认生产根 API 重导出 `VenvManager` 与 `SkillFileFingerprint`，其中前者只供跨 crate 集成测试使用，后者实际由 Tools 经 `haven_skills` 直接引用。

## 决定

- 将 watcher 的 fingerprint、比较状态和“变化后才刷新”操作收回 `SkillRegistry`。跨 crate 操作使用 `refresh_if_changed() -> Result<bool>`；Skills owner 在刷新成功后才提交新观察状态，失败时保留旧状态供下一轮重试。
- 保留 Tools watcher 的轮询节奏、取消、Tools catalog rebuild 和 change notification；移除跨 crate fingerprint 类型与查询 API。
- 将 `haven_skills::venv` 设为私有实现模块，只从 Skills crate root 导出生产工具执行所需的 `VenvManager`。
- 删除没有消费者的 `VenvManager::root()`。在 `haven_tools` 中只于 `test-support` feature 下重导出 `VenvManager`，供 Agent 集成测试构造 `SkillRunner`；生产执行仍由 Tools 内部直接使用 Skills root API。
- 从 Tools crate root 删除未被消费的 `SkillFileFingerprint` re-export。
- 保留 `SkillRegistry`、`ToolRegistry`、`OperationRegistry` 的不同 owner 与职责；名称后缀相同不足以合并不同生命周期的集合。

## 影响

这是尚未发布 workspace 内部 API 的破坏性收口，不保留旧 watcher API 或实现模块路径。轮询间隔、首次刷新、发现新增/修改/删除技能、刷新失败重试、工具 catalog rebuild 及 UI 通知行为保持不变。无 IPC、配置、持久化或执行安全语义变化。

## 验证

- 静态搜索确认 workspace 不再使用 `list_skill_file_fingerprints`、`SkillFileFingerprint` 公共类型或 `haven_skills::venv` 模块路径。
- 增加 registry 行为回归覆盖：首次刷新、无变化不刷新、文件新增/修改/删除触发刷新。
- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo test --workspace --locked -- --test-threads=1`
- `cargo clippy --workspace --locked -- -D warnings`
- ADR 索引检查。

## 替代方案

- 只将 Tools 的 fingerprint 向量改成私有字段：拒绝，owner 和变化判断仍在 Tools，Skills 扫描表示仍跨边界。
- 把扫描逻辑复制到 Tools：拒绝，会复制对根目录和 symlink containment 的过滤策略。
- 让 Skills 返回 fingerprint 并要求消费者自行比较：拒绝，Tools watcher 不是 fingerprint 的业务 owner。
- 因都叫 Registry 而统一 Skills、Tools 和 Operation registry：拒绝，分别表达发现/enablement、可执行工具注册和延迟操作/生命周期状态。

## 回滚

如需恢复旧机制，可重新导出 `SkillFileFingerprint` 并恢复 `list_skill_file_fingerprints()`，将比较状态移回 Tools watcher；但这会重新引入被本 ADR 关闭的实现 DTO 边界。`VenvManager::root()` 仅在出现明确消费者时恢复，并优先提供语义所需的窄操作。
