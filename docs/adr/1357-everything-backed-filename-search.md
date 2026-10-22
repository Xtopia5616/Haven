# ADR 1357：通过 Everything CLI 扩展文件名搜索

## 状态

Accepted — 2026-10-22

## 背景

原生 `files.search` 文件名模式需要遍历目录树；Windows 用户可能已安装 Everything 并维护全局文件名索引。Everything 的官方 ES CLI 可通过 Everything IPC 查询该索引。Everything 不搜索文件内容，且其索引结果不能替代 Haven 的实时文件授权。

## 决定

1. `files.search` 的 `filename` 模式可选择调用用户安装的 voidtools ES CLI；`content` 模式继续使用 Haven 原生搜索。默认关闭，未配置时保持原生行为。
2. 用户在仅本地的 `[everything]` 配置段设置 `enabled` 与 `command`。命令必须是绝对本地路径。该可执行配置不进入 `SettingsSnapshot`、Tauri IPC 或日志 Debug；设置页的普通写入不能覆盖它。命令直接启动，不经过 shell，启动前清理继承环境并仅恢复平台运行基线。
3. 每次请求将已授权并解析的搜索根以独立参数传给 `-path`，查询经 `-search` 参数传递；固定使用 JSON 与全路径输出选项。启动前要求 proof-bound Full Access 与可用网络策略，使用 Windows Job Object 管理进程树。最多并发两个查询，子进程等待上限 15 秒，Everything 数据库等待 5 秒，stdout 上限 4 MiB、stderr 保留上限 16 KiB。取消时终止子进程。
4. ES 输出是非可信索引数据。每条路径必须位于搜索根内、可规范化且通过现有 `FileReadPolicy` metadata 打开检查；越权、失效或重解析点路径会剔除并使 `search_complete` 为 false。结果数沿用请求上限，额外请求一条用于判断 `has_more`。
5. ES 不存在、Everything 未运行、超时、输出无效或命令失败时返回可操作错误，不静默切换成可能语义不同的原生遍历。未启用 Everything 时保持原生实现。
6. 不捆绑或下载 Everything/ES。ES 上游仓库声明 MIT；`THIRD_PARTY_LICENSES.md` 记录可选外部依赖及官方许可证来源。Everything 主程序由用户另行安装，集成只使用 ES 官方 CLI。
7. 现有 filename `pattern` 在 Everything 启用时按 Everything query syntax 解释；原生后端继续按原有匹配语义运行。结果通过 `search_backend` 区分，工具指引说明这一差异。
8. ES 的 `-timeout` 仅等待数据库加载，不证明首次索引已完成。零条结果必须报告 `search_complete=false`、`index_coverage="unknown"`，并提示检查 Everything 索引范围和索引状态；Haven 不将其解释为已确认无匹配项。
9. Everything 命中在授权检查后通过与原生 Walker 同配置的 `IncrementalIgnore` matcher，遵守 `max_depth`、`ignore_hidden`、层级 ignore 文件、Git 全局 ignore 与 `.git/info/exclude`。

## 替代方案

- 默认自动探测 Everything：拒绝，安装/运行状态和命令查找会产生机器相关的隐式后端切换。
- 将 ES 或 Everything 二进制纳入 Haven 安装包：本切片拒绝，以免增加上游版本、架构、签名、升级和再分发责任。
- 将 Everything 用于内容搜索：拒绝，其索引按文件名与路径工作。

## 影响与兼容性

- 现有配置缺少 `[everything]` 时使用关闭默认值；无数据库、IPC 或 Settings shape 变化，不需要重置。
- 用户必须安装并运行 Everything，另行取得 ES CLI，并在 `config.toml` 指定 ES 路径。Everything 索引配置与索引内容由 Everything 管理。
- Everything CLI 进程与 Everything 主程序在当前用户权限下运行；进程回收和 Haven 路径过滤不构成 Windows 文件系统隔离。
- 单次搜索语义遵循 Everything 查询语法；原生 filename glob 与 Everything query syntax 并不完全相同，结果会在 `search_backend` 标出来源。
- 非空结果只表示索引中存在命中；Everything 索引不保证覆盖 Haven 搜索根中全部文件。零结果的覆盖状态始终标为未知。

## 验证

- Common 配置验证覆盖未启用、缺命令、控制字符和 Debug 脱敏；配置 round-trip 与 Settings 隔离测试覆盖本地可执行字段。
- Tools 测试覆盖 JSON 数组/逐行输出、路径根组件边界和有界 reader；Common 测试覆盖配置校验、脱敏与 Settings 隔离。
- 运行 `cargo fmt --all -- --check`，`cargo test --locked -p haven-common`，以及 `cargo test --locked -p haven-tools`。真实 Everything/ES 查询需要 Windows 上安装 Everything 和 ES 的手工验收。
- 用户配置、索引范围、故障排查和真实 Windows 验收步骤见 [`everything-search.md`](../everything-search.md)。

## 回滚

删除 Everything config source、适配器和本 ADR；`config.toml` 的 `[everything]` 段需要在回滚前删除，因为旧版本拒绝未知字段。数据库与会话数据不受影响。
