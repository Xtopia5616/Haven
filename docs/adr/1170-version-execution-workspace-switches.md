# ADR 1170：按代次切换执行工作区

## 状态

已采纳并实施（Tools 后端 owner 与执行上下文）。renderer IPC、选择器和跨重启持久化仍未实施。

## 背景

ADR 1167 的 `ExecutionWorkspace` 只在启动时发现一次，generation 固定为 `1`。若之后允许切换项目根，Files、Shell、授权器和 Agent prompt 必须对每次 attempt 使用同一工作区代次；旧 proof 不能被带到新根上。Shell 的目录只在启动子进程时生效，Files 的写权限还需要继续绑定到授权时打开的目录句柄。

## 决策

- `AuthorizationEngine` 是活动执行工作区和 generation 的唯一 owner。app-facing `ToolsFacade::switch_execution_workspace` 只接受已存在的本地目录；Tools 规范化根并打开目录 capability 后再替换。不存在、非目录、UNC/device path 或不能安全打开的目标拒绝，失败时旧快照保持不变。
- 选择当前 canonical 根是幂等操作，不增加 generation。成功切换到不同根时，在 owner 写锁内分配单调递增 generation 并增加 policy revision；generation 溢出 fail closed。旧 execution proof 随 policy revision 一同失效。
- `AuthorizedExecutor` 为 proof-verified attempt 捕获单一 `ToolExecutionContext`。该 context 在一次 policy revision 读锁内同时带有 workspace root/generation 与 Files 写入 capability。Files 相对路径解析和 Shell 默认 `cwd` 都使用该 attempt 的快照；显式 Shell `cwd` 仍优先。
- 一旦 attempt 捕获并开始执行，切换不尝试重定向或回滚它。它继续使用原 generation：Files 持有的目录句柄固定原根；Shell 在创建进程时已确定 `cwd`。切换完成后，新 attempt 必须取得新 proof 和新快照。该语义不把 Shell 变成 OS filesystem sandbox。
- 选择只作用于当前进程，不写配置或数据库；启动时仍使用既有仓库根发现规则。此切片新增的是 Tools app-facing operation，没有 renderer 命令、事件或 UI 选择器。

## 替代方案

- 让 Files、Shell 和 prompt 各自读取当前 workspace：拒绝，会允许同一 attempt 混用根代次。
- 根切换后原地修改旧 `ExecutionWorkspace`：拒绝，已捕获的 proof/context 可能观察到部分更新。
- 把旧 proof 自动迁移到新 generation：拒绝，旧授权不能自动代表对新项目根的授权。
- 由 renderer 直接传路径而不由 Tools 打开和规范化：拒绝，renderer 不能成为后端权限根的 owner。

## 影响与迁移

当前不增加配置、IPC 或持久化字段，无需数据迁移或重置。app 可通过 `ToolsFacade` 切换当前进程 workspace；尚未接入 UI，也不会跨应用重启保留选择。已有授权 proof 在 workspace 改变后需要重新授权/生成；永久与会话 grant 本身不被删除，仍受新 workspace 和 live policy 边界约束。

运行中 attempt 使用切换前的快照并可能完成一次已获授权操作。调用方若要求切换后没有旧 attempt 尚在运行，需要另行定义 turn/tool drain 生命周期；本 ADR 不宣称切换会中止或回滚已开始的文件系统/进程副作用。

## 验证

- 新测试覆盖 canonical 根切换递增 generation、重复选择幂等、无效目录拒绝且不改旧快照，以及旧 proof 在 workspace switch 后失效。
- Shell 测试覆盖 attempt context 的新默认 cwd 生效、显式 cwd 仍优先；Files 路径测试继续覆盖快照内的相对路径和 unavailable 拒绝。
- `cargo fmt --all -- --check`、`cargo check --locked --offline -p haven-tools` 和 `cargo clippy --locked --offline -p haven-tools -- -D warnings` 通过。
- `cargo test --locked --offline -p haven-tools`：901 passed、2 ignored；MCP integration：7 passed。工作区切换的定向回归测试也单独通过。
