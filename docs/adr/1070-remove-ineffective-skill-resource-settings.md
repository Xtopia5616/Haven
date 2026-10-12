# ADR 1070：删除无效的 Skill 资源限制配置

## 状态

已接受（2026-10-13）

## 背景

`skills_exec.cpu_time_secs` 和 `max_memory_mb` 出现在配置与 Settings 中，但 Windows 进程运行器没有施加这两项限制。展示为可配置项会让用户误以为执行受它们保护。

## 决定

- 从 Rust 配置 DTO、默认值、生成的 UI contract 与 Settings parser 中删除 `cpu_time_secs` 和 `max_memory_mb`。
- 不增加伪装成有效的 clamp；当前被实际执行的 timeout、输出上限与进程隔离保持原 owner。
- 旧配置含任一字段时按 `deny_unknown_fields` 失败：保留原文件备份并以默认配置启动。用户可仅删除这两行保留其余设置。

## 影响

配置 key shape 有意破坏性收窄；不涉及数据库或 IPC command shape，无需重置数据库。

## 验证

- Loader 回归确认旧字段触发备份、文件不被覆盖且 `skills_exec` 回退默认值。
- Common 与 UI contract 测试覆盖字段删除后的配置 shape。
- `docs/release-and-reset.md` 已说明如何手动修复旧配置。
