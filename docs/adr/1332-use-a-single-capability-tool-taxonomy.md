# ADR 1332：统一工具目录的能力分类

## 状态

Accepted — 2026-10-19

## 背景

`ToolCatalogGroup` 同时承载能力领域与执行来源：Skills 和 MCP 各自占一类，尽管它们是工具来源；`ask`、`notify`、加载入口和实际能力也缺少清晰、统一的归类。Provider 是否常驻直供或按需加载是可见性/加载状态，不属于能力类别。

## 决定

`ToolCatalogGroup` 表示且只表示能力分类。每个模型工具在此字段中只有一个类别：

| 分类 | 能力范围 |
|---|---|
| `haven` | Haven 记忆、诊断、配置与管理操作 |
| `interaction` | 向用户提问和发送通知：`ask`、`notify` |
| `workflow` | `agent.*`、`session.*`、`toolrun.*`、`schedule.*` 协作与生命周期操作 |
| `files` | 文件系统和授权工作区文件操作 |
| `desktop` | 窗口、输入与剪贴板操作 |
| `system` | 系统信息、设备、电源与环境操作 |
| `media` | 媒体、语音、录音、播放和本机音频操作 |
| `network` | Web 搜索与批准的 HTTP 请求 |
| `shell` | 授权执行工作区中的命令 |
| `extensions` | 工具目录/加载器及 Skill、MCP 扩展工具 |
| `other` | 新增能力尚无明确类别时的兜底值 |

`ToolSource` 继续独立表达 builtin、Skill 或 MCP 来源及其安全边界；按 source 过滤目录时，工具 family 仍取自能力分类。Provider 常驻直供与按需加载仍由既有 provider surface 和 catalog 状态表达，不新增分类值。

## 替代方案

- 把 `direct` 作为 `ToolCatalogGroup`：拒绝。它描述可见/加载方式，不能说明工具提供什么能力，并会与 `ToolSource` 一样混淆分类维度。
- Skills 与 MCP 各自占目录类：拒绝。两者是来源形式，不是稳定能力领域；实际 Skill/MCP 工具统一归入 `extensions`。
- 按工具名在 UI 推导类别：拒绝。Agent prompt、目录查询和设置页继续共享后端拥有的唯一分类。

## 影响与回滚

Rust 到 TypeScript 的 `ToolCatalogGroup` wire 值从 `skills`、`mcp` 调整为 `interaction`、`extensions`，Rust 与 UI 必须同版本发布并同步生成契约。变化只影响展示/提示词路由元数据，不改工具名、来源、授权、执行、配置、数据库或 durable event，因此无需数据重置。回滚时恢复旧枚举、旧 family 映射和 UI 标签并重新生成 IPC 契约。

## 验证

验证覆盖分类摘要、provider 工具分类、catalog family 查询、Workflow 操作分组、设置页分类分组与显示标签，以及 Rust→TypeScript IPC 漂移。
