# ADR 1160：采用 Agent Skills 的 `SKILL.md` 格式

## 状态

已采纳并实施（2026-10-14）

## 背景

Haven 原先使用自定义的标题和 Metadata 列表解析 Skill 清单，无法直接读取 Agent Skills 生态中的 `SKILL.md`。官方格式要求 YAML frontmatter 与 Markdown 正文，并定义了名称、描述及可选字段的约束。Skill 清单属于用户可编辑输入，解析必须保留 Haven 既有的字节、行数和路径安全上限。

## 决定

- 按 Agent Skills 官方规范解析 YAML frontmatter：要求 `name` 与 `description`，校验名称格式、目录名称匹配、描述长度以及可选 `compatibility` 长度；正文作为 Markdown 指令，不要求特定标题。
- 接受 `license`、`compatibility`、`metadata` 与实验性 `allowed-tools`。Haven 从 `metadata.version` 和 `metadata.haven.language` 读取自身扩展。其它可选字段只解析，不改变执行语义；`allowed-tools` 不绕过或替代 Haven 的后端授权策略。
- 使用 `serde-saphyr` 反序列化受限 frontmatter，并继续执行既有的文件大小、解析行数、单行字节和路径边界检查。
- `skill_create` 生成标准 frontmatter。源码中的旧格式 Skill fixture 全部转换为新格式；文档给出旧版迁移说明。
- 保留 Windows 设备名拒绝和大小写冲突拒绝，这是文件系统安全约束，不属于 Agent Skills 的通用名称规则。

## 替代方案

- 继续使用 Haven 自定义格式：放弃，不能直接消费标准 Skill。
- 手写 YAML 子集解析器：放弃，容易与 YAML 字符串转义及类型规则不一致。
- 同时长期接受新旧两种格式：放弃，增加双格式解析和测试面，并让旧契约继续存在；测试版的用户 Skill 可按文档一次性手工转换。

## 影响

新的 `SKILL.md` 与 Agent Skills 格式兼容。已有用户 Skill 文件不会自动改写；旧格式会显示清单错误且不会执行，用户需按 `docs/skills.md` 转换。Skill 文件位于用户 Skills 目录，不属于数据库或应用配置，因此无需重置数据库或配置，也不会有自动写入用户目录的迁移。增加一个仅用于 frontmatter 反序列化的 workspace 依赖。

## 验证

- 对照 [Agent Skills 官方格式](https://agentskills.io/specification)实现 frontmatter 字段约束，并将 Rust 测试 fixture 转换为标准格式。
- `cargo test --locked -p haven-skills`：50 项通过。
- `CARGO_PROFILE_TEST_DEBUG=0 cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings` 和 `cargo fmt --all -- --check` 全部通过。完整 workspace 测试也覆盖 Tools 的 `skill_create` 输出与 Agent 的 Skill 加载路径。

## 回滚

若需回滚，恢复旧解析器和旧版 `skill_create` 模板，同时将已转换的源码 fixture 恢复为旧格式。用户手工转换的 Skill 可依据 `docs/skills.md` 中的迁移示例还原；本决定不涉及持久数据、数据库 schema 或应用配置重置。
