# ADR 0880：收回 Skill manifest 解析与未校验构造 API

## 状态

Accepted — 2026-10-10

## 背景

`parse_skill_md`、`SkillManifest` 和 `Skill::from_manifest_unchecked` 曾构成 Skills crate 的公开 API。调用图显示，manifest parser 只在 Skills crate 内部使用；manifest 类型在 workspace 中仅被测试通过 unchecked constructor 构造。该构造器以 `#[doc(hidden)]` 隐藏文档，但仍编译进生产库，允许调用者绕过清单解析、名称校验与 Registry 发现步骤，直接伪造可传给 SkillRunner 的 `Skill`。

`Language::parse` 也只由内部 parser 使用；相反，Tools SkillRunner 确实调用 `Language::as_str()` 生成不支持语言的错误文本，因此后者仍是跨 crate API。当前没有 parser、manifest 或 unchecked constructor 的仓内生产消费者，Haven 也不承诺这些内部 Rust API 的向后兼容。

## 决定

- 将 `SkillManifest` 及其字段、`parse_skill_md` 和 `Language::parse` 收回 Skills crate 内部；Tools 不再重导出 `SkillManifest`。
- 删除生产构建中的 `Skill::from_manifest_unchecked`。生产 `Skill` 只能由 Skills 目录扫描器从已校验的 `SKILL.md` 发现。
- Skills、Tools 与 Agent 的现有测试夹具通过临时目录、真实 manifest 与 `SkillRegistry` 创建 Skill；runner fixture 的入口脚本仍通过目录扫描发现。
- 保留 `Language::as_str()`：Tools SkillRunner 用它向用户报告不支持的 manifest 语言，存在实际跨 crate 消费者。
- 保留 `Skill` 和已确认的跨 crate getter；不改变 allowlist、manifest 解析、路径校验、执行或工具注册的生产行为。

## 影响与兼容性

只收窄 Haven Skills crate 的内部 Rust 构造与解析 API；IPC、工具输出、配置、持久化和运行时执行行为不变，无需重置。不保留旧方法或类型 alias。测试夹具改为走真实临时目录扫描，测试套件未运行。

## 验证

通过：`cargo fmt --all -- --check`、`cargo check --workspace --all-targets --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、新 ADR 的 Prettier 检查及 `git diff --check`。workspace 全 target 编译只编译测试目标，不执行测试；Rust 与 UI 测试套件未运行。全目标 Clippy 初次检查发现了此前未清理的测试代码告警，已在独立清理切片修复并复跑通过。

## 回滚

若未来某个生产层确实需要单独解析或构造 Skill，应明确输入校验、路径授权和状态 owner 后再定义窄 API；不要恢复绕过扫描校验的 `from_manifest_unchecked`。
