# ADR 0824：让无效 Skill 清单在目录中可诊断

## 状态

Accepted — 2026-10-09

## 背景

`parse_skill_md` 会拒绝无效 metadata，但 `scan_dir` 只记录 warning 并丢弃目录。Skills Registry 因此只保存成功解析的 skill，Skills 页面拿到空数组时无法区分“没有安装 skill”和“清单解析失败”。原解析逻辑还会把缺少 `- ` 的普通行当作 metadata 内容继续尝试解析，导致 Metadata 约束不明确。

## 决定

- `## Metadata` 区块内每个非空内容行必须匹配 `- key: value`；仅支持 `name`、`description`、`version` 和 `language`。自由文本、HTML 注释、缺少冒号或空 key 都是带行号的解析错误。
- Registry 分别保存可执行 skill 与无效清单诊断。目录列表将无效项投影为 `enabled: false`，并提供 `load_error` / `disabled_reason`；它们永远不会进入运行时 Skill map、工具目录或执行路径。
- 有效 skill 的目录项也提供不可执行原因：配置 allowlist 禁用或缺少受支持的入口脚本。App Skills IPC、Skills 页面和 `skills_list` 工具共享该诊断投影。
- `docs/skills.md` 是 Metadata 格式与列表诊断的用户/开发者文档入口。

## 替代方案

- 只保留日志 warning：拒绝。UI 无法解释空目录列表，操作者必须改查本机日志。
- 把解析失败的 Skill 插入运行 registry 并只依赖 `enabled: false`：拒绝。无效 manifest 没有可靠的运行身份与指令，不能暴露到执行路径。
- 把整个 `SKILL.md` 错误行原文返回：拒绝。错误信息只说明格式与行号，避免把任意清单内容回显到日志/UI。

## 影响与验证

Tauri `SkillInfo` 增加可选原因字段，IPC TypeScript 类型由 Rust DTO 生成；工具 `skills_list` 对诊断字段采用 optional omission。Skills 页现在能看到解析失败以及有效 Skill 的停用原因。没有持久数据或配置格式变化，无需重置。

适用门禁：IPC 生成/漂移检查、Rust workspace 编译与严格 Clippy、前端类型检查。测试因本次执行约束未运行；Windows Skills 页面行为仍需桌面会话验收。

## 回滚

回滚时删除 `SkillInfo` 诊断字段、Skills 页面原因呈现和无效清单列表投影，并恢复 Metadata 的旧解析行为。仅影响进程内目录快照与 IPC；不需要数据或配置重置。
