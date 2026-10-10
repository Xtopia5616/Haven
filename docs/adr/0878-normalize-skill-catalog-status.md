# ADR 0878：正交化 Skill 目录诊断与可执行状态

## 状态

Accepted — 2026-10-10

## 背景

ADR 0824 为无效 `SKILL.md` 增加诊断目录项，并在 DTO 中使用 `load_error` 与 `disabled_reason`。当前实现将同一解析错误写入两个字段；目录项同时投影 `has_script: false`，使 UI 误显示“缺少入口脚本”。`load_error` 也过于宽泛：实际错误只来自清单解析，脚本执行错误由 ToolResult/执行路径单独负责。

继续审查发现 `Skill.enabled` 表示配置 allowlist，而 `SkillInfo.enabled` 又被改成“allowlist 启用且存在脚本”。UI 同时用这个值表示设置开关和执行状态，缺少脚本但仍在 allowlist 中的技能因此被显示为停用，并锁死了用于关闭配置的开关。Agent prompt 和 `execute_skill` 也需要明确检查可执行性，而不是读取配置开关。

## 决定

- 将 `SkillInfo.load_error` 和工具 `skills_list` 的 `load_error` 统一改名为 `manifest_error`，它只表达 `SKILL.md` 解析失败，不表示脚本加载或执行失败。
- `SkillInfo.enabled` 与 `Skill.enabled` 统一表示 allowlist 配置状态；新增 `SkillInfo.executable` 表示当前是否可执行（已配置启用且有入口脚本）。Agent prompt 与 `execute_skill` 按 `executable` 筛选/授权；UI toggle 按 `enabled` 反映和修改配置状态。
- 无效清单诊断项设置 `manifest_error`，不设置 `unavailable_reason`，并将 `enabled` / `executable` 设为 false；此时 `has_script: false` 是不可知清单的占位，不作为缺少脚本的证据。UI 显示清单无效且禁用修改入口。
- 有效清单只有在 allowlist 禁用或缺少入口脚本时设置 `unavailable_reason`。缺少脚本但配置仍启用的条目显示两种事实，并允许用户关闭该配置；尚无脚本且当前已禁用时，UI 不允许发出必然失败的启用请求。
- App IPC、Skills 页、`skills_list` 和诊断工具输出以及 UI runtime validator 使用统一字段与语义；两个工具输出共用 `SkillCatalogStatusOutput`，需要目录路径的 `skills_list` 只在该 projection 外附加 root；不保留 `load_error` 或 `disabled_reason` alias。

## 影响与兼容性

这是测试期 Haven 自有 IPC 与工具输出契约的直接替换，不提供旧字段兼容。无持久化数据、用户配置或数据库 schema 变化，无需重置。历史 ADR 0824 保留当时字段名作为决策记录；当前状态轴与字段契约以本 ADR 为准。

## 验证

通过：IPC contract generation/check（79 handlers）、Rust workspace check 与 strict Clippy、Svelte type check（0 errors / 0 warnings）、Rust formatting、ADR/Skills 文档 Prettier、`git diff --check`。Rust 与 UI 测试套件未运行；Windows 页面行为仍需桌面会话验收。

## 回滚

若回滚本次契约，恢复 `load_error` 和旧的 `enabled` 投影；这会重新引入字段职责重叠、配置/可执行状态混淆及无效清单的脚本状态误报，因此应由新的 ADR 明确替代，不应添加双字段兼容期。
