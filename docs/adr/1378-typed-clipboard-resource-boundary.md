# ADR 1378：剪贴板 typed 资源边界

## 状态

已实施 — 2026-10-23。

## 背景

剪贴板工具已有独立的 read、write、history 操作视图，但统一授权只知道 capability，无法区分对系统剪贴板的读取和写入，也无法把写入所需的文件或受管图片来源放进确认和持久授权范围。

## 决定

1. 声明单一系统剪贴板资源及 `read` / `write` 方向。`clipboard.read` 与 `clipboard.list_history` 声明只读；`clipboard.write` 声明写入。
2. 图片写入额外声明精确的受管 `asset_id`；文件列表写入额外声明每个输入路径为本地 `read` 资源。文本与 HTML payload 不构成额外资源。
3. 确认 receipt、proof 和执行上下文绑定完整 typed 资源。剪贴板 adapter 在副作用前要求资源与 proof 完全相等，并持有当前策略 revision 的执行 permit。文件列表路径按同一 proof 中捕获的文件读取策略检查；相对路径按本次执行工作区解析。
4. Session/Always 资源范围支持按剪贴板访问方向及具体受管资产 ID 匹配；已有本地路径资源仍负责限制文件列表来源。剪贴板本身不新增第二套可配置 allowlist。

## 配置与数据影响

数据库 schema 与 session event 不变。配置中的 `PermissionResourceScope` 增加剪贴板方向和受管资产 ID 两种可序列化范围；旧配置格式无需迁移或重置。IPC 的资源判别联合新增对应形状，UI 使用明确的中文说明展示它们。

## 替代方案

- 只按工具名授权整个剪贴板：拒绝；读取剪贴板不应隐式批准写入。
- 图片或文件 payload 只绑定 `clipboard.write`：拒绝；持久授权必须一并绑定来源资产或文件路径。
- 为剪贴板增加与文件/网络并列的配置字段：拒绝；方向与来源范围已可由同一 typed resource 与 scoped permission 表达。

## 验收与限制

测试覆盖 read/history 与 write 方向声明、图片 ID 和文件路径来源、无效资产 ID/歧义 payload 拒绝、持久授权方向隔离及资产精确匹配。文件路径执行前经当前读取策略的受句柄校验；Windows 原生剪贴板端到端行为仍纳入 Windows 验收门禁。
