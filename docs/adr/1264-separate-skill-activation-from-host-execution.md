# ADR 1264：区分 Skill 激活与宿主进程执行边界

## 状态

已采纳并实施。

## 背景

`load_skill` 只将已启用的 Skill adapter 注册到当前 session 的工具 overlay；它不启动子进程，也不访问网络。但通用 operation 分类把 `load_skill` 与 `load_mcp` 一起标为 `HostProcess + Opaque`，导致 `WorkspaceWrite` 和 `NetworkPolicy::Deny` 都会拒绝这项本地工具激活操作。

真正执行 Skill 的 `skill__*` adapter 会进入 `SkillRunner`：`VenvManager::ensure` 可运行 `python -m venv` 和 `pip install`，随后启动 Skill 的 Python 脚本。脚本拥有当前用户进程的文件/网络权限；filtered environment、Job Object 和临时 `cwd` 不是 OS 文件系统沙箱。

当前 HCS execution port 只接受 `cmd` / PowerShell Shell request，输入 workspace 是项目 `ExecutionWorkspace` 的有界快照。它没有固定 Python runtime、Skill root 输入、venv/requirements 依赖闭包或 Skill 输入输出隔离契约。把 Skill 包装成一条 Shell command 不会补齐这些边界。AppContainer 现有后端也没有每 attempt 的运行期磁盘硬配额，不能替代 HCS quota gate。

## 决定

- 精确的 `load_skill` operation 标为 `ExecutionBoundary::InProcess` 与 `NetworkAccess::None`。它的风险、session scope、effect 和 confirmation 规则不变。
- Skill 执行 adapter（`skill__*`）继续标为 `HostProcess + Opaque`。在 `WorkspaceWrite` 下于 handler 前拒绝；FullAccess 下仍经过既有 Skill 执行授权、进程启动 permit、进程树 containment、取消与输出上限。
- 不把 Skill adapter 标为 `WorkspaceIsolated`，也不通过 Shell 字符串转发执行。
- 要在 `WorkspaceWrite` 开放 Skill 执行，后续必须先定义 typed guest contract，至少固定 guest Python runtime、Skill 文件输入、依赖/离线包闭包、参数和文件 capability、网络策略、磁盘/时间/输出预算、取消及输出审阅行为；所有数据继续经现有 workspace/review owner 流转。

## 替代方案

- 继续把 `load_skill` 按宿主进程和网络调用拒绝：会让无副作用的 session surface 激活受到无关边界限制，拒绝。
- 将所有 Skill execution 设为进程内或 workspace isolated：Python 脚本仍是任意代码，当前 runner 不能证明宿主文件或网络访问受到 OS 限制，拒绝。
- 将 Skill 脚本拼成 Shell 命令交给 HCS：Shell port 不固定 Python、Skill 内容、依赖或参数的 typed 边界，且会扩大命令解析面，拒绝。

## 影响与回滚

在 `WorkspaceWrite + NetworkPolicy::Deny` 下，模型可以加载 Skill adapter 到 session surface；实际调用仍被 `SandboxBoundary` 拒绝，不会创建 venv、启动 pip 或启动 Skill Python 进程。operation policy 中 `load_skill` 的网络与执行边界字段发生变化，但没有持久配置、数据库或 renderer wire schema 变更，无需数据重置。回滚时恢复 `load_skill` 的 `HostProcess + Opaque` 分类即可；Skill 执行边界无需变化。

## 验证

- 通过真实 `ToolsFacade` 授权解析确认 `load_skill` 在 `WorkspaceWrite + Deny` 下得到 `InProcess + None` 并通过授权。
- 注册真实 Skill adapter，再确认其 `Opaque + HostProcess` 在 `WorkspaceWrite` 下于 handler 前被拒绝。
- `cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、ADR 索引和 crate 依赖门禁按本切片执行。
- 本 ADR 不运行真实 HCS 或 guest 超额写入验收；该验收仍是独立发布 Gate。
