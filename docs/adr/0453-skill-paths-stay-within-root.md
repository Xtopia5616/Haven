# ADR 0453：限制 Skill 文件链接的目录边界

- 状态：Implemented
- 日期：2026-10-04
- 范围：Skill discovery、目录指纹和入口脚本解析
- 关联：ADR 0015、0145

## 背景

Skill 扫描器会 canonicalize Skill 目录，但读取 `SKILL.md` 时沿用未解析的目录项路径；入口脚本只检查 `exists()`。因此目录中的文件链接/reparse point 可以指向 Skill 目录外：扫描器会读取外部 manifest，runner 也会执行外部脚本。自动刷新目录指纹也会跟随外部 manifest 链接。

## 决定

1. 读取 manifest 前 canonicalize 目标，并要求其位于当前 Skill 目录内。
2. 解析入口脚本时 canonicalize 根目录和候选目标；只有根目录内的普通文件可执行，并返回已解析路径。
3. 目录指纹忽略指向 Skill 目录外的 manifest，避免跟踪未授权的外部目标。
4. 普通文件以及 Skill 根目录内的链接目标继续可用；外部链接不作为 Skill manifest 或入口脚本。

## 影响

文件链接和 Windows reparse point 的目标现在必须留在 Skill 目录内。数据库、配置、工具 wire、provider 和用户数据格式不变，无需重置。技能根目录内部链接仍可读取/执行；对已配置为外部链接的技能，清单将跳过，或其入口脚本显示为不可用。

## 验证

回归测试覆盖外部 `SKILL.md` 与脚本链接拒绝；Windows 测试在系统不允许创建符号链接时跳过链接场景。运行 `haven-skills` 测试及 workspace 安全门禁。

## 回滚

删除 manifest/脚本目标的 canonical path 边界检查会恢复目录外文件读取与执行。若确有外部资源需求，应先定义显式受信任根目录和授权策略，不应恢复无约束的链接跟随。
