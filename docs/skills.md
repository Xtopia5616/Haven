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

Skill 根目录最多扫描 1024 个条目；超限时刷新失败并保留上一次完整列表。可选 `requirements.txt` 最大为 1 MiB，必须解析到 Skill 根目录内的普通文件；超限或链接到根目录外时环境准备失败，且不会启动 pip。

## `skill_create` 默认 Python 脚手架

创建 skill 时可以省略 `script`，Haven 会生成 `scripts/main.py` 入门模板。模板通过 `sys.stdin.buffer.read()` 读取输入，并用 `utf-8-sig` 解码后解析 JSON object；`run(params)` 应返回结构化字典，推荐把机器可用结果放在 `hashes`、`detected` 等字段，把人读说明放在 `report`。

入口会将 `run(params)` 的标准输出重定向到内存缓冲区，避免调试 `print()` 污染协议；进程最终只向 stdout 输出一行 JSON envelope，成功包含 `ok: true`，异常包含 `ok: false` 与 `error: {type, message}`。不要向 stderr 写日志：skill runner 将非空 stderr 或非零退出视为执行失败。模板保证正常 Python 异常也走 JSON 错误 envelope；语法错误、解释器启动失败等发生在脚本入口执行前的错误仍由 runner 报告。
