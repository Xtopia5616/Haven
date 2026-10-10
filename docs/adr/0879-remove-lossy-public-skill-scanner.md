# ADR 0879：删除丢弃目录诊断的公开 Skill 扫描器

## 状态

Accepted — 2026-10-10

## 背景

`haven-skills` 曾同时暴露 `scan_dir` 与 `SkillRegistry::refresh_from_disk` 使用的内部扫描器。前者只返回 `Vec<Skill>`，把无效 `SKILL.md` 的诊断丢掉；后者返回 `SkillScanResult`，同时保留可执行候选与目录诊断。全仓调用图中没有 `scan_dir` 的生产消费者，调用都位于 Skills crate 自身的扫描测试。

两个入口让调用者无法从 API 名称判断是否会保留诊断，也为新消费者提供了静默降低信息完整性的路径。项目仍处于测试阶段，且不要求 Haven 自有 API 向后兼容。

## 决定

- 删除公开 `scan_dir`，不保留 alias 或兼容包装。
- 将唯一生产扫描实现命名为 `scan_skill_directory`，返回完整 `SkillScanResult`；`SkillRegistry::refresh_from_disk` 同时消费 skills 与 diagnostics。
- 扫描测试使用仅在 `cfg(test)` 下存在的 `scan_parsed_skill_entries` 投影，并以 `skill_directory_scan_*` 命名相关测试；该 helper 只代表成功解析的 registry 项，不断言其中每项都有脚本，也不可被生产消费者引用。
- 目录发现、规范化路径安全检查、collision 规则、allowlist 语义和错误日志行为保持不变。

## 影响与兼容性

仅移除 Haven Skills crate 的内部 Rust API；仓内生产调用者为零，不影响 Tauri IPC、工具输出、配置、持久化或执行行为，无需重置。不提供向后兼容入口。若未来有另一个 crate 确实需要扫描，应让它消费完整结果，或先定义由唯一目录 owner 提供的、不会隐去诊断的专用契约。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、新 ADR 的 Prettier 检查及 `git diff --check`。全文件 Prettier 检查确认 `docs/naming.md` 与路线图在本次改动前已不符合该工具输出；保留既有长表格排版，仅核对新增规则与路线图条目。Rust 与 UI 测试套件未运行。

## 回滚

若未来扫描消费者需要另一种视图，应通过完整 `SkillScanResult` 明确投影，并记录调用者如何处理诊断；不要恢复会静默丢弃诊断的 `scan_dir -> Vec<Skill>`。
