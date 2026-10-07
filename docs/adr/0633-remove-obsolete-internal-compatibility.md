# ADR 0633：移除过时的 Haven 内部兼容入口

## 状态

已采纳并实施。

## 背景

项目级命名审计发现几处仍主动接受旧 Haven 格式或同义入口：fact/summary extraction marker decoder 会补齐历史缺省字段；`CacheDiagnostics` 会把缺失 metadata 填成默认值；`MediaProbe` 的 Rust 字段已叫 `media_kind`，Serde 却继续写 `media_type`；工具目录筛选接受未列入当前契约的复数 source 名；STT chat fallback 将未声明的 audio capability 当作 supported。它们增加了隐式契约，让当前字段名称与持久格式无法从定义本身判断，也会让未知能力被当成可用能力。

外部服务协议、Windows 字符编码和不同领域的结果词汇也有相似的“兼容/别名”字样，但它们承担当前互操作、安全或投影语义，不能按历史 Haven 数据清理的规则机械删除。

## 决定

1. fact extraction marker 只接受完整的 `event_sequence:bypass_throttle:attempt:next_attempt_at_ms`；拒绝单字符布尔值和缺少 retry 字段的短格式。
2. summary extraction marker 只接受完整的 `session_id:attempt:next_attempt_at_ms`；拒绝仅有 session ID 的 value-only 格式。现有 malformed marker quarantine/repair 保留，作为当前契约的损坏数据恢复机制，不是旧格式迁移。
3. `CacheDiagnostics` 反序列化要求全部当前 metadata 字段；删除缺字段默认补值。因其 JSON 会存进 `llm_usage` 与 session event，数据库 schema 升至 v38；不迁移 v37 数据，升级时按发布说明重建数据库。
4. `MediaProbe` 序列化与反序列化统一使用 `media_kind`、`mime_type`。审计确认它没有落入数据库或 Tauri IPC contract，因此不额外要求配置或数据库重置。
5. Tool catalog 的 `source` filter 只接受 `all`、`builtin`、`skill`、`mcp`；未公开的 `builtins` / `skills` 同义输入被拒绝。
6. `ToolObservationOutcome::succeeded` 与 ToolRun 状态 `completed` 分属 Agent 结果和持久任务生命周期。UI 将前者映射为共享卡片展示状态 `completed`，保留该转换，不视为旧字段兼容。
7. 保留由当前外部契约要求的格式：MCP `inputSchema` wire key、各 LLM provider 的响应变体，以及 Windows 工具的非 UTF-8 输出解码。`ToolManifestSource` 保留对未来 source token 的开放解析；它是 forward extension policy，不读取旧 Haven 存储格式。
8. STT chat fallback 仅在当前 adapter 明确声明支持 audio 时发送原始音频；`CapabilitySupport::Unknown` 由 planner 拒绝。测试 client 显式声明能力，不再依赖默认 profile 被提升。
9. Messaging envelope 的 `thread_id` 是当前可选字段：没有 thread context 时使用 `None`，这是当前数据模型语义，不是旧格式转换。Resume 工具结果 helper 使用 `persistedToolOutcome`，避免把“历史”误解成兼容路径。

## 验证

- 对 fact/summary marker 与 `CacheDiagnostics` 增加旧格式拒绝、当前格式接受测试；schema 测试继续检查版本拒绝与新建库。
- 对 `MediaProbe` 增加 Serde key 正向和旧 key 拒绝测试；对 catalog source 筛选检查 canonical 名与复数名拒绝。
- 运行 Rust workspace 测试、严格 Clippy、UI 检查/测试/构建和 IPC drift check。

## 重置与回滚

从 v37 升级必须退出 Haven 并删除 `%APPDATA%\haven\haven.db`、`haven.db-wal`、`haven.db-shm`；其它开发环境按 `docs/release-and-reset.md` 的路径处理。无需删除 `config.toml`，除非配置内容同时包含 ADR 0632 列出的旧 `llm.models[].provider` key。v38 数据库不由 v37 二进制打开；回滚只能使用升级前备份并按旧版本重建其数据根目录。
