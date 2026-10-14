# ADR 1167：建立后端进程级 ExecutionWorkspace

## 状态

已采纳并实施；Files 目录句柄锚定见 [ADR 1169](1169-anchor-file-mutations-to-authorized-directories.md)，按代次切换 workspace 见 [ADR 1170](1170-version-execution-workspace-switches.md)，配置持久化见 [ADR 1171](1171-persist-execution-workspace-in-security-settings.md)。原生目录选择器、scratch 生命周期和进程级文件隔离仍在路线图 Active。

## 背景

文件工具和 AuthorizationEngine 分别在每次调用时从进程当前目录推导仓库根；Agent prompt 又自行计算并展示 workspace 根。三者可能因为启动目录、规范化或更新时机不同而对同一相对路径作出不同解释。

同时，`SecurityConfig.sandbox_mode` 默认是 `workspace_write`，但 `writable_roots` 为空时，授权器没有对带路径的写操作施加默认根限制。Shell、Skill 与 MCP stdio 的 cwd 又有各自来源，不能拿它们替代文件权限边界。

## 决策

- 在 `haven-tools` 内部由唯一 AuthorizationEngine 持有只读 `ExecutionWorkspace` 快照。快照从进程启动目录通过现有仓库根发现规则解析；根必须规范化为本地绝对目录。不能解析或发现到根时记为 unavailable，不回退到未经发现的当前目录或 Temp。
- 快照具有进程内 generation。启动发现或设置应用的根必须由后端 owner 打开；根替换增加 generation，并更新策略 revision 以废止旧 execution proof。
- `FilesTool` 相对 path/root/destination 与授权器相对路径检查使用同一快照；Agent prompt 从 Tools facade 的只读快照展示 `workspace_root`，不再自行推导第二份根目录。
- `workspace_write` 下，显式 `writable_roots` 非空时只允许这些 roots；为空时以 `ExecutionWorkspace.root` 作为默认写入根。根不可用且请求带写入路径时 fail closed。该检查仍受工具 `allowed_paths`、`read_only`、live policy 和每次 execution proof 复核约束。
- 该 workspace 是后端执行上下文，不是 renderer 的页面/tab workspace。它不改变 Shell `cwd`、Skills `work_dir`/venv、MCP stdio `cwd`、MCP 远端目标或 UI 导航状态，也不声称 Job Object 能约束子进程文件系统。
- 当前路径沙箱仍是授权入口处的 reparse-aware path preflight；它不能关闭检查与文件写入之间的所有路径竞态。受信文件读取继续使用已有的打开句柄身份校验。写入目录句柄锚定、workspace 生命周期/切换、scratch 范围和 Windows OS filesystem isolation 另行实现与验收。

## 替代方案

- 每个调用方继续各自推导路径：拒绝，会保留路径解释漂移。
- 把 renderer 当前导航 workspace 直接作为权限根：拒绝，UI 可变状态不能扩大后端权限。
- 当根缺失时退回进程 cwd 或 Temp 并允许写入：拒绝，启动目录并非已授权 workspace，Temp 也不是项目写入根。
- 把 Job Object 当成 filesystem sandbox：拒绝，它负责回收进程树，不限制子进程读写路径。

## 影响与迁移

本切片不改变 database schema、持久化 session/tool-run 数据、IPC payload 或配置字段，因此无需数据迁移或数据库/配置重置。用户可见的权限变化是：`workspace_write` 且 `writable_roots` 为空时，内建带路径写操作现在默认限于发现的执行根；根不可用时写入被阻止。明确配置的 `writable_roots` 保留原有精确范围语义，`full_access` 仍是显式逃生模式。

## 验证

- 覆盖同一注入 workspace 下的相对文件路径解析、workspace 内绝对/相对写路径允许进入确认、workspace 外路径拒绝，以及原有 `allowed_paths` 相对路径回归。
- `FilesTool`、AuthorizationEngine 与 Agent prompt 都从同一 Tools owner snapshot 取得根目录。
- `cargo check --workspace --all-targets --locked --offline`、`cargo test --locked --offline -p haven-tools`（897 passed，2 ignored）、`cargo test --locked --offline -p haven-app-binary`（249 passed）及 Agent prompt workspace adapter 定向测试通过。
- 全 workspace 测试仅在已有的 `haven-skills` 工作区改动上有两个失败：不支持语言 frontmatter fixture 解析，以及无效 registry diagnostic 的唯一名称计数；本切片未修改这些文件。严格全 workspace Clippy 也被既有 Messaging/LLM lint 阻止；对这些已存在 lint 显式放行后全 workspace Clippy 通过。
- Windows 发布验收仍需覆盖 canonical local path、reparse point 与竞态；这一切片的进程内路径边界不构成 OS sandbox 通过证明。
