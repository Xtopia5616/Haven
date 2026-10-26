# Agent 的 LSP 配置与使用

Haven 可以让 Agent 通过本机语言服务器查询代码定义、实现、类型定义、引用、悬停信息、文档符号、工作区符号和诊断。LSP 能力由用户安装并配置的语言服务器提供；Haven 不会自动安装或发现服务器。

## 配置语言服务器

编辑正式版配置 `%APPDATA%\haven\config.toml`，添加一项 `[[lsp_servers]]`。例如 Rust：

```toml
[[lsp_servers]]
name = "rust-analyzer"
command = "rust-analyzer"
args = []
extensions = [".rs"]
language_id = "rust"
enabled = true
```

`command` 必须能从 Haven 的运行环境启动；如果服务器需要参数，将每个参数分别写入 `args`，不要把整条命令写成 shell 字符串。`extensions` 决定按文件查询时选择哪个服务器；每种扩展名只能配置给一个服务器。`name` 用于工作区符号查询，且在配置中唯一。

保存配置后重启 Haven。配置错误、服务器未安装或扩展名没有匹配服务器时，Agent 会收到操作错误。

## Agent 如何调用

LSP 操作通过 `discover_tools` 按需发现和加载。Agent 可以先按 `lsp` 搜索，再查看并加载需要的具体操作；常用名称如下：

- `lsp.search_workspace_symbols`：按名字在工作区搜索，不需要文件位置。传入配置中的 `server` 名称和 `query`。
- `lsp.list_document_symbols`：列出一个文件中的声明及其范围。
- `lsp.find_definition`、`lsp.find_implementations`、`lsp.find_type_definition`、`lsp.find_references`、`lsp.get_hover`：对指定文件和源码位置查询。
- `lsp.get_diagnostics`：查询指定文件的语言服务器诊断。

位置操作的 `line` 和 `character` 都从 1 开始；`character` 使用 UTF-16 代码单元计数。若 Agent 不知道符号所在文件或位置，可以先用 `lsp.search_workspace_symbols` 搜索名字，再用返回的位置继续导航。服务器必须支持工作区符号请求，结果取决于其语言索引能力。

示例请求：

```json
{"server":"rust-analyzer","query":"SessionActor"}
```

## 授权与限制

语言服务器是本机可执行程序，属于高风险进程操作，受统一授权和确认策略管理。Windows 的 Workspace Write（或经确认扩展的 ReadOnly）使用 MXC，项目与运行时依赖只读，写入限于受管 scratch。Restricted 网络按需申请；任意语言服务器无法逐域名约束，因此申请界面明确显示实际子进程网络范围，包括私网与宿主 loopback。Full Access 使用宿主进程权限；显式 Deny Network 仍禁止联网。用户拒绝、取消、授权过期或隔离不可用时不启动服务器。

每次操作都会启动并退出一次服务器，不复用后台进程或索引。单次工具调用默认最多等待 60 秒，单文档限制为 1 MiB，结果限制为 2 MiB。受管 scratch 为 256 MiB、最多 100,000 条目，运行中检查容量并在退出后清理。大项目或启动较慢的服务器可能超时；同一轮需要多次查询时，当前实现会重复启动服务器。

配置和安全边界的架构决定见 [ADR 1340](adr/1340-authorize-local-language-servers.md) 、[ADR 1341](adr/1341-expand-lsp-navigation.md) 与 [ADR 1428](adr/1428-permission-first-resource-consent.md)。
