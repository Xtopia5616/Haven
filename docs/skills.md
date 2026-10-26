# Skills 清单格式与诊断

Haven 的 `SKILL.md` 按照 [Agent Skills 官方格式](https://agentskills.io/specification)解析：YAML frontmatter 后接 Markdown 正文。Skill 目录名必须与 frontmatter 的 `name` 相同。格式错误或名称不匹配的目录会显示在 Tools 页并带有清单解析错误，但不会注册为可执行 Skill。

## Frontmatter

`name` 和 `description` 是必填字符串。名称最长 64 个字符，仅允许小写 ASCII 字母、数字和单连字符；不能以连字符开头或结尾，也不能包含连续连字符。名称还必须与所在目录名完全一致。由于 Haven 运行在 Windows 上，Windows 保留设备名也会被拒绝。`description` 为 1–1024 个字符，需说明 Skill 的用途以及适用场景。

官方可选字段 `license`、`compatibility`、`metadata` 和实验性 `allowed-tools` 均可解析。`compatibility` 最长 500 个字符。Haven 使用 `metadata.version` 展示 Skill 版本，用 `metadata.haven.language` 选择运行时语言（当前只有 `python` 可执行）；其余 metadata 和可选描述字段暂不影响执行。`allowed-tools` 目前只按格式读取，不授予或限制 Haven 工具权限；工具授权仍由 Haven 的后端策略决定。

```markdown
---
name: file-organizer
description: Organizes files by type and date. Use when asked to sort or clean up a folder.
license: MIT
metadata:
  version: "1.0.0"
  haven.language: python
---

# File Organizer

Describe the skill behavior here. The body may use any Markdown structure.
```

## 从旧版 Haven 格式迁移

旧版 Haven 使用 `# Skill:` 和 `## Metadata`。该格式不再读取；已有自定义 Skill 需手动改写为 YAML frontmatter。把旧的 `version` 移到 `metadata.version`，把 `language` 移到 `metadata.haven.language`，并确认 `name` 与目录名相同。旧版格式的目录会保留在磁盘上，但会显示为无效清单，不会执行。

```markdown
---
name: example
description: A short description of what this skill does and when to use it.
metadata:
  version: "1.0.0"
  haven.language: python
---

Describe the skill behavior here.
```

正文是 frontmatter 之后的完整 Markdown，不要求 `## Instructions` 标题。可选资源目录遵循 Agent Skills 惯例，包括 `scripts/`、`references/` 和 `assets/`；Haven 当前只执行 `scripts/main.py` 或 `scripts/<name>.py`，并不自动加载引用文件。

模型通过 `load_skill` 激活 Skill 时，成功结果会带上所选 Skill 的完整正文（`skill_instructions`），供当前对话后续步骤按任务使用。正文与状态一起进入有界工具观察；若所选正文无法完整放入 `load_skill` 的有效输出上限（受全局 `max_observation_chars` 及更低的单工具 `max_output_chars` 约束），Haven 会拒绝这次激活并提示缩短正文或减少选择，不会把截断正文当作已加载指令。

`manifest_error` 只表示 `SKILL.md` 解析失败；有效清单因 allowlist 或入口脚本不可用时，由 `unavailable_reason` 说明。无效清单的 `has_script: false` 表示脚本状态未知，不能据此推断磁盘上没有入口脚本。

Skill 根目录最多扫描 1024 个条目；超限时刷新失败并保留上一次完整列表。可选 `requirements.txt` 最大为 1 MiB，必须解析到 Skill 根目录内的普通文件；指纹与 pip 安装使用同一份有界文件快照，requirements 超限或越界时不会启动 pip。入口脚本最大为 16 MiB；执行前从已校验句柄复制到临时快照，保留原脚本路径与同目录模块导入行为；入口脚本超限或越界时不会启动 Skill 脚本。

每次 Skill 调用在 `skills_exec.work_dir` 配置的绝对本地目录下获得独立临时工作目录。相对路径文件不会在不同调用间共享，正常完成、失败、超时或取消后都会尝试清理；Windows 句柄释放有短暂延迟时会有限重试。Haven 异常退出可能遗留目录；后续 Skill 调用会在最多检查 4096 个直接子项的范围内，尝试回收超过执行期限加 60 秒、符合 Haven 随机目录名且未被跨进程 owner 锁占用的目录，每次最多删除 16 个。无法确认的目录会保留；超过检查上限时本次 Skill 执行失败关闭。目录仅用于临时文件组织，不会限制脚本访问工作目录以外的文件或网络；当前安全策略仍要求 `FullAccess + Open` 才能运行这类 opaque 子进程。脚本应通过约定的 JSON stdout 返回结构化结果；cwd 中生成的文件在调用结束时删除。

## `skill_create` 默认 Python 脚手架

创建 Skill 时可以省略 `script`，Haven 会生成 `scripts/main.py` 入门模板，并写出符合 Agent Skills 格式的 `SKILL.md`。模板通过 `sys.stdin.buffer.read()` 读取输入，并用 `utf-8-sig` 解码后解析 JSON object；`run(params)` 应返回结构化字典，推荐把机器可用结果放在 `hashes`、`detected` 等字段，把人读说明放在 `report`。

入口会将 `run(params)` 的标准输出重定向到内存缓冲区，避免调试 `print()` 污染协议；进程最终只向 stdout 输出一行 JSON envelope，成功包含 `ok: true`，异常包含 `ok: false` 与 `error: {type, message}`。不要向 stderr 写日志：skill runner 将非空 stderr 或非零退出视为执行失败。模板保证正常 Python 异常也走 JSON 错误 envelope；语法错误、解释器启动失败等发生在脚本入口执行前的错误仍由 runner 报告。

## 进程权限

Skill 脚本及 venv/pip 准备进程共用后端授权 owner。Windows Workspace Write 与经确认扩展的 ReadOnly 使用 MXC；Restricted 网络不足时申请实际子进程网络范围，不再默认拒绝。任意 Python 进程无法逐域名约束，申请会明确包含公网出站、私网与宿主 loopback。用户拒绝、取消或执行证明失效时不启动。Skills 的启用开关只决定是否加载能力，不授予文件或网络访问权。详见 [ADR 1428](adr/1428-permission-first-resource-consent.md)。
