# ADR 1171：在安全设置中持久化 ExecutionWorkspace

## 状态

已采纳并实施（`SecurityConfig`、Tools 授权 owner、完整 Settings IPC 与安全设置路径字段）。原生目录选择器仍待实现。

## 背景

ADR 1170 增加了进程内 workspace 切换，但重启后仍从启动目录重新发现根，也没有应用设置入口。用户需要能为 Files 相对路径和默认 Shell `cwd` 指定项目根，同时不能让 renderer 成为 workspace capability 的 owner。

## 决策

- `SecurityConfig.execution_root: Option<PathBuf>` 是执行根配置的唯一持久来源，与 `writable_roots` 分开。`None` 表示沿用启动仓库发现；显式目录影响 Files 相对路径、默认 Shell `cwd`，并在 `writable_roots` 为空时成为 `workspace_write` 的默认写入根。
- 安全设置页编辑该路径，随完整 `Settings` 经既有 IPC 保存。保存前 app 通过 `ToolsFacade` 试开本地目录 capability；Common 配置校验还要求路径绝对。它不接受 renderer 导航状态作为权限根，也不添加接受任意路径的 Tauri 命令。
- 设置应用时 `AuthorizationEngine` 在同一 owner 写锁内替换 workspace、策略与 grant 快照。切换 workspace 会增加 generation；策略 revision 变化使旧 proof 失效。已开始的 attempt 保留其捕获的旧 workspace；新 attempt 使用新 generation。
- 若启动时持久化的目录已经不存在或不能安全打开，应用继续启动并应用其余安全设置，但将 workspace 置为 unavailable、记录错误，Files 相对路径和 workspace 默认写入失败关闭；不回退到仓库根或 cwd。设置页保存时对同类问题提前拒绝。
- 该 workspace 只统一路径解析和默认 cwd，不是 Shell、Skill 或 MCP 子进程的 OS 文件系统隔离。UI 明确展示这一限制。

## 替代方案

- 把执行根写入 `writable_roots`：拒绝，两者职责不同；显式 writable roots 可有多个，并覆盖默认写入范围，不应顺带改变相对路径解析或 Shell `cwd`。
- 由 renderer 每次执行时传根目录：拒绝，会让调用方绕过 Tools 的唯一权限 owner，并导致同一 attempt 的路径解释漂移。
- 持久化路径失效时静默回退：拒绝，回退会把授权从用户指定的项目重定向到另一目录。
- 假设目录当前存在就等同于操作系统 sandbox：拒绝，Shell/Skill/MCP 子进程仍能访问该进程的其它系统路径。

## 影响与迁移

`execution_root` 是新增可选 TOML 字段，旧配置缺省为 `None` 并继续使用原启动发现规则；不改数据库 schema、session 事件或工具 ID，无需重置数据库。用户可在安全设置中填写现有本地绝对目录并保存。路径字段目前为文本输入；原生目录选择器后续单独接入。

显式 `writable_roots` 仍独立限定 workspace write；当列表为空时，以 ExecutionWorkspace 作为默认范围。`full_access` 的显式语义不变。切换应用后旧授权 proof 失效，已有运行尝试不会被重定向或回滚。

## 验证

- Common 校验拒绝相对执行根。
- Tools 测试覆盖配置根切换增加 generation、清空配置恢复启动发现，以及配置根失效时 workspace 变为 unavailable。
- Settings command 在提交设置前试开目标目录；UI 测试覆盖路径字段与 writable roots 独立。
- `cargo check/test/clippy`、IPC contract generation、UI check/test/build 按跨 crate 与 Rust/UI IPC 门禁执行；Windows 系统隔离验收仍属于路线图后续工作。
