# ADR 1066：为提示历史与外部缓冲配置建立硬上限

## 状态

已接受（2026-10-12）

## 背景

若干资源型配置只做 JavaScript safe-integer 检查，Settings 控件也使用通用的大数上限。调用方可把响应缓冲、技能说明、目录列表等容量设到实际执行路径无法安全承受的值。`prompt_history_limit` 也缺少与 Settings 范围一致的上限。多个容量控件把 KiB/MiB 显示成 KB/MB，计量单位与二进制换算不一致。

## 决定

- `session.prompt_history_limit` 的有效范围为 0–500 条；0 不附带历史消息。
- `context_limits.file_max_list_entries` 的上限为 10,000 条；0 返回空列表。
- `context_limits.mcp_max_sse_buffer_bytes` 的上限为 4 MiB；0 丢弃所有 SSE 事件。
- `context_limits.skills_max_md_bytes` 的上限为 1 MiB；0 跳过所有 `SKILL.md`。
- `context_limits.network_max_body_bytes` 的上限为 16 MiB；0 不保留 HTTP 响应体。
- 以上上限由 Common 同时校验配置文件和 Settings，UI parser 与数字控件复用相同边界。容量控件的二进制换算统一标为 KiB/MiB。

## 影响

这些范围匹配当前默认值和消费端的资源用途。旧配置中高于上限的值会按既有配置错误策略备份整份 `config.toml` 并在本次进程使用默认值；将数值调回范围即可保留其它配置。Settings 越界更新会被拒绝。零值的既有“禁用/空结果”行为保留。配置 key、IPC shape 和数据库 schema 均未变化，不需要重置数据库。

## 验证

- Common 测试覆盖各范围边界、配置文件和 Settings 拒绝行为。
- UI contract 测试覆盖同一组上下界；Settings 控件使用导出的共享范围。
- 运行受影响的 Common/UI 门禁，并将全量工作区门禁留到本轮跨域切片完成后统一复核。
