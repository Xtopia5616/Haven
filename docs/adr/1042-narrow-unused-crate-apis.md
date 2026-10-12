# 1042：收回未使用的 crate API 与重复入口

## 状态

已采纳并实施（2026-10-12）。

## 背景

对 crate 根与 facade 的真实工作区消费者核对发现，少量公开方法只供 crate 自身测试或内部调用；另有 `DetectedMediaKind` 的第二个公共导入路径。将这些入口保留为默认生产 API 会扩大可绕过 owner 的表面，并使实现或测试辅助能力看起来像稳定跨 crate 契约。

## 决定

- `ConfigService::new` 仅在 Common 单测构建可见；无工作区消费者的 `ConfigService::load` 与 `ConfigLoader::settings` 删除。生产继续通过组合根注入 credential store，配置快照读取由 `ConfigService::settings` 唯一拥有。
- `Skill::version` 留作 Skills 内部 DTO 投影；保留 `Skill::instructions`，因为 parser 将作者提供的 instructions 作为 Skill contract 数据保留，删除会改变其读取语义。原始 `SkillRegistry::get_skill` 只在 Skills 单测或显式 `test-support` feature 下可用；Tools 的存在性检查改读 owner 提供的 `SkillInfo`。
- `ToolsFacade::catalog_version_for_session` 收窄为 crate 内接口；`set_tool_enabled` 私有化。仅测试调用的 admin-context 注入、context-limit getter 与 TTS 状态 getter 仅在测试构建暴露。
- `DetectedMediaKind` 只从 `haven_common::media_detection` 导入；`haven_common::media` 中不再提供重复公共别名。
- 保留有实际消费者的 `list_skills`、技能执行 getter、工具 catalog snapshot、ConfigService typed settings 与 Common 领域子模块。

## 影响

改动只影响 Haven workspace 内部 crate API；没有数据库、配置或 IPC wire 数据变化，不需要数据重置。测试替身相关的 raw Skill getter 通过 Skills 的非默认 `test-support` feature 提供，普通生产依赖不含该入口。

## 验证

- `cargo fmt --all -- --check`
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked`

## 回滚

若发现工作区内新的真实消费者，应在 owner crate 增加窄的领域查询/操作，再迁移该消费者；不恢复宽泛 raw getter 或重复 Common 根/子域别名。
