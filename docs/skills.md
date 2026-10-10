# Skills 清单格式与诊断

Skill 目录应包含 `SKILL.md`。清单由 `haven-skills` 解析；格式错误的目录会显示在 Tools 页并带有清单解析错误，但不会注册为可执行 skill。`enabled` 表示 allowlist 配置，`executable` 表示当前能否执行；两者不会因缺少入口脚本而混为一谈。

`manifest_error` 只表示 `SKILL.md` 解析失败；有效清单因 allowlist 或入口脚本不可用时，由 `unavailable_reason` 说明。无效清单的 `has_script: false` 表示脚本状态未知，不能据此推断磁盘上没有入口脚本。

## Metadata 格式

`## Metadata` 下的每个非空内容行必须使用 `- key: value` 格式。允许字段为 `name`、`description`、`version` 和 `language`。不要在此区块内放 HTML 注释、自由文本或其它 YAML/Markdown 语法；解析器会把它们作为格式错误显式报告。注释和说明文字应放在 `## Instructions` 或 Metadata 区块之外。

示例：

```markdown
# Skill: example

## Metadata

- name: example
- description: A short description.
- version: 1.0.0
- language: python

## Instructions

Describe the skill behavior here.
```

有效 skill 的不可执行原因也会显示在列表中，例如配置 allowlist 将其禁用，或缺少 `scripts/main.py` / `scripts/<name>.py` 入口脚本。配置已启用但缺少脚本时，列表会分别说明配置状态和当前不可执行状态；可以关闭该配置，但不能在入口脚本缺失时启用。
