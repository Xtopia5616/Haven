# ADR 1264：区分 Skill 激活与宿主进程执行边界

## 状态

已采纳并实施。

## 背景

`load_skill` 只将已启用的 Skill adapter 注册到当前 session 的工具 overlay；它不启动子进程，也不访问网络。但通用 operation 分类把 `load_skill` 与 `load_mcp` 一起标为 `HostProcess + Opaque`，导致 `WorkspaceWrite` 和 `NetworkPolicy::Deny` 都会拒绝这项本地工具激活操作。

真正执行 Skill 的 `skill__*` adapter 会进入 `SkillRunner`：`VenvManager::ensure` 可运行 `python -m venv` 和 `pip install`，随后启动 Skill 的 Python 脚本。脚本以当前用户权限运行；filtered environment、Job Object 和临时 `cwd` 不限制它访问的文件或网络。

Skill 执行启动宿主 Python 子进程，现有工作区写入规则无法约束子进程自身的文件访问。

## 决定

- 精确的 `load_skill` operation 标为 `ExecutionBoundary::InProcess` 与 `NetworkAccess::None`。它的风险、session scope、effect 和 confirmation 规则不变。
- Skill 执行 adapter（`skill__*`）继续标为 `HostProcess + Opaque`。在 `WorkspaceWrite` 下于 handler 前拒绝；FullAccess 下仍经过既有 Skill 执行授权、进程启动 permit、进程树 containment、取消与输出上限。

## 替代方案

- 继续把 `load_skill` 按宿主进程和网络调用拒绝：会让无副作用的 session surface 激活受到无关边界限制，拒绝。
- 将 Skill execution 设为进程内：Python 脚本仍是宿主代码，当前实现不能约束其文件访问，拒绝。

## 影响与回滚

在 `WorkspaceWrite + NetworkPolicy::Deny` 下，模型可以加载 Skill adapter 到 session surface；实际调用仍被 `SandboxBoundary` 拒绝，不会创建 venv、启动 pip 或启动 Skill Python 进程。operation policy 中 `load_skill` 的网络与执行边界字段发生变化，但没有持久配置、数据库或 renderer wire schema 变更，无需数据重置。回滚时恢复 `load_skill` 的 `HostProcess + Opaque` 分类即可；Skill 执行边界无需变化。

## 验证

- 通过真实 `ToolsFacade` 授权解析确认 `load_skill` 在 `WorkspaceWrite + Deny` 下得到 `InProcess + None` 并通过授权。
- 注册真实 Skill adapter，再确认其 `Opaque + HostProcess` 在 `WorkspaceWrite` 下于 handler 前被拒绝。
- `cargo fmt --all -- --check`、`cargo test --locked -p haven-tools`、`cargo clippy --locked -p haven-tools -- -D warnings`、ADR 索引和 crate 依赖门禁按本切片执行。
