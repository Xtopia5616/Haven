# ADR 1340：通过授权的本机 LSP 提供代码智能

## 状态

Accepted

## 背景

Agent 可以读取文件，但无法可靠地按语言查询定义、引用、悬停信息、文档符号和诊断。将每种语言的解析器直接实现到 Haven 会重复语言工具链；把任意语言服务器作为普通 MCP/Skill 配置又会丢失面向文件的协议与工作区约束。

语言服务器是用户配置的可执行程序。它能以 Haven 用户身份访问本机文件并自行联网；Windows Job Object 进程 containment 只能回收进程树，不能限制文件或网络访问。执行它必须沿用工具授权证明、安全策略复核和本机进程 containment。

## 决定

1. 在 Tools 内实现 Agent-facing `lsp.*` operations：`definition`、`references`、`hover`、`document_symbols` 和 `diagnostics`。输入路径只允许活动执行工作区内的文件；工具通过 Files read policy 读取 UTF-8 文档，并以标准 file URI、UTF-16 行列位置与 LSP JSON-RPC 通信。
2. 在 `AppConfig.lsp_servers` 中配置服务器，按唯一名称、唯一扩展名、语言 ID、命令和参数描述服务器；默认空集合使未配置环境保持可用。配置 owner 是 Common `ConfigService`。可执行命令与参数通过自定义 `Debug` 脱敏，不进入 Settings snapshot / IPC，也不由 Settings patch 改写。

```toml
[[lsp_servers]]
name = "rust-analyzer"
command = "rust-analyzer"
args = []
extensions = [".rs"]
language_id = "rust"
enabled = true
```

3. 每次工具调用按扩展名解析服务器，直接启动该程序（不经过 shell），将工作目录设为活动执行根，清空环境后仅恢复现有 runtime baseline；进程使用 platform `ProcessContainment` 和 `kill_on_drop`。每个执行器最多并发两个进程。每次调用创建短期 LSP 会话并在成功后发送 shutdown/exit。
4. 以有界、UTF-8 的 Content-Length framing 实现 LSP stdio；请求、通知、pull/push diagnostics 和常见 workspace/client 请求由 Tools 私有协议模块处理。旧服务器的 push diagnostics 最多等待 5 秒。单文档上限 1 MiB，协议消息上限 16 MiB，序列化结果上限 2 MiB，工具调用 deadline 60 秒。
5. 每个 LSP operation 视为 High risk、UserData、Opaque network、HostProcess 与非只读 effect，并沿用常规工具授权和确认。用户配置的可执行程序可能执行任意副作用，因此按 NonIdempotent 处理，自动重放被禁止。启动前再持有 policy permit 并复核当前 proof、workspace generation、sandbox 与网络模式；只允许 Full Access，Network Deny/Restricted 明确拒绝。Full Access 且 Ask/Open 的联网许可由工具授权流程决定。Workspace Write 下不启动 LSP。
6. 不增加 UI 配置页、Settings IPC 字段、数据库表或持久 LSP 进程池。用户将 `[[lsp_servers]]` 写入 `config.toml`；独立调用和每次进程回收降低了服务器配置更新、crash recovery 与长生命周期同步的复杂度。

## 考虑过的替代方案

- **内置语言解析器：拒绝。** 会为 Haven 增加语言特定依赖与不同步的索引语义。
- **把服务器当 MCP/Skill 管理：拒绝。** MCP 面向外部工具 schema，Skill 面向脚本；二者都不拥有文件文档同步和 LSP 请求语义。
- **驻留服务器并跨调用复用：暂不采用。** 这会要求管理 workspace/doc 生命周期、配置热更、崩溃重启和取消后的会话隔离。当前 one-shot 做法更易限制资源与回收。
- **允许服务器在 Workspace Write 中运行：拒绝。** 进程 containment 不提供只写工作区的 OS 文件系统边界；服务器可在 Haven 用户权限下任意读写。
- **在 Settings 页编辑 executable command/args：拒绝。** 这些字段属于可执行配置，不应进入 renderer 的设置 snapshot 或一般 UI DTO。

## 影响与兼容性

- 默认不配置服务器时，LSP 工具会返回可操作错误，既有使用不变。
- 缺少 `lsp_servers` 的现有 `config.toml` 使用默认空集合；数据库、IPC shape 与持久化数据不变，不需要重置。
- 配置的命令与参数可能含有敏感信息，Debug 不输出具体值，Settings IPC 不包含该字段。
- 服务器进程仍拥有 Haven 用户的 OS 权限。Full Access 与确认是用户授权边界，Job Object 不是 filesystem/network sandbox。
- 每次操作都会启动一次服务器，适合短查询；对启动开销敏感的语言服务器可能较慢。
- 改动 `lsp_servers` 配置后重启 Haven 以载入新快照；配置的进程命令不经 Shell 解析。

## 验证

- Common 测试覆盖配置 round-trip、Settings snapshot 隔离、验证规则与 Debug 脱敏。
- Tools 测试覆盖 LSP frame、服务器发起的 workspace 配置请求、超限拒绝、工具策略、路径 traversal 与 UTF-16 坐标。
- 运行 Rust workspace fmt/check/strict Clippy/tests；变更不修改 Tauri IPC 或 UI。
- Windows 验收仍需确认 `ProcessContainment` 对服务器进程树的创建与回收行为。

## 回滚

删除 `lsp.*` tool、Common `lsp_servers` 配置字段、工具配置 source 与本 ADR/架构说明即可。没有数据库或 UI 状态需要迁移；配置含 `lsp_servers` 时，回滚版本会因严格配置契约拒绝该字段，用户可从 `config.toml` 删除对应段落或按发布说明重建配置。
