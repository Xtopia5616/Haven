# ADR 1367：简化工作区写入模式的进程边界

## 状态

已采纳并实现。

## 背景

为在 `WorkspaceWrite` 下运行宿主子进程，项目增加了 Windows 容器 broker、配额验收、workspace 拷贝/回传、隔离输出审阅和文件晋升链路。该方案无法在当前受支持环境完成正式验收，且实现规模远超这项模式所需的最小行为。Skill 管理页也曾为复用输出审阅 UI，把一次预览包装成 durable 会话步骤；Shell 状态 UI 又增加了单独 IPC 查询。

## 决定

- 删除未落地的 Windows 容器执行、配额验收、隔离输出审阅与晋升功能，以及 broker 服务、专属配置、IPC 和 UI。
- `read_only` 与 `WorkspaceWrite` 下不启动无法由 Haven 文件授权约束的宿主子进程，包括 Shell、Skill 和 MCP stdio。受控文件操作继续经 Tools 授权与执行工作区检查。
- `FullAccess` 下获准的宿主子进程以当前 Windows 用户权限运行；`NetworkPolicy` 管理 Haven 的工具授权，不是系统防火墙。
- Skill 管理操作使用现有直接命令，不伪装成 ReAct 会话步骤。MCP stdio 使用固定的本机 launcher。
- 不保留容器配额发布门禁；本仓库不再声称提供 WorkspaceWrite 子进程隔离。

## 影响与迁移

不改数据库 schema。旧 `config.toml` 若 MCP server 条目含 `stdio_execution`，严格配置解析会备份并回退默认配置；保留其它设置时，手动删除该字段即可。无需删除数据库。旧二进制不支持读取本次新契约。

## 验证

已验证：`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI 类型检查/测试/生产构建、IPC contract/event 检查、ADR 索引与格式检查。Windows 宿主进程授权仍按现有 Tools 策略执行；不再有 HCS 配额 gate 或其正式验收结论。

## 回滚

恢复旧实现会重新引入当前已删除的配置与 IPC 契约，并可能造成设置重置；不恢复容器服务或系统功能状态。
