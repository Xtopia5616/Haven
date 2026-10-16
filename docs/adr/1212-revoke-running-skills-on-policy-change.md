# ADR 1212：策略撤销时终止运行中的 Skill 进程树

## 状态

已实现。前台 Skill 脚本、venv 创建和 pip 安装受 proof 绑定的撤销域约束；每个 setup 子进程在启动时重新取得短期 permit。全局策略变化仍全局撤销；工具设置和 session trust 撤销按 ADR 1217/1218 定向停止进程。App 直接执行 Skill 的路径也必须把后端签发的 execution proof 交给 Tools owner。

## 背景

[ADR 1211](1211-revoke-running-shells-on-policy-change.md) 让前台和后台 Shell 在策略变化后终止受管进程树，但 Skill 仍只响应调用取消。Skill 的副作用还包括入口脚本，以及 `VenvManager` 按 Skill manifest 创建 venv、运行 pip 安装依赖的子进程。只保护主脚本会留下策略撤销后 bootstrap 继续执行的窗口。

另外，Tauri 的 `execute_skill` 命令此前先检查权限，再调用一个没有 execution proof 参数的 Skill port；Tools 内部虽然隐藏了 registry，但这个调用链不能证明实际执行与检查过的 capability 和输入一致。Skill adapter 的裸 `execute` 与 `VenvManager::ensure` 也提供了不带授权上下文的进程启动入口。

## 决定

- Skill adapter 只接受 proof-derived `ToolExecutionContext`。裸 `Tool::execute` fail closed；Tools 的 `SkillExecutionPort` 校验请求没有 session/target binding，且 tool name、完整参数和 external High policy 与目标 Skill 精确匹配，然后验证 proof 并在 Tools owner 内解析可执行 Skill。
- App 直接执行 Skill 时使用 `authorize_for_execution`。自动批准分支将 proof 交给 Tools；需确认时只排队 request/receipt，确认被接受后由后端 `confirm_for_execution` 签发 proof。proof 不进入 Tauri payload 或 renderer。
- SkillRunner 在等待执行容量、固定脚本快照、准备 venv、启动主脚本和等待主进程时监听全局策略 signal、工具设置 token 和 session trust token。对应撤销返回 `Cancelled`，并终止主脚本及其 Job Object 进程树。
- `VenvManager::ensure` 要求调用方提供窄 `ProcessStartAuthorization`。venv 创建和 pip 安装各自在 OS spawn 前取得从同一执行 proof 派生的短期 policy read permit；permit 覆盖 spawn 与 containment attach/resume，随后释放，不锁住整个安装过程。
- 如果策略变化发生在 venv/pip 子进程运行期间，SkillRunner 丢弃 setup future；`kill_on_drop` 和 containment owner 回收该子进程树，已验证的管道/进程树回收测试覆盖 future drop 行为。
- 全局 `policy_revision` 继续拒绝旧 proof；permission mode、执行边界和永久 trust 规则变更仍取消所有绑定旧 signal 的 Skill/Shell。工具设置与 session trust 依据 ADR 1217/1218 只取消受影响工具或 session；授权 grant 不撤销已运行工作。MCP stdio 尚未接入本生命周期。
- 本切片只补授权证明、实时撤销与进程启动线性化。Skill 仍不是 AppContainer 文件系统 sandbox；进程环境过滤、私有工作目录或 Job Object 都不代表 OS filesystem/network sandbox，也不改变 Skill 现有 OS 能力。

## 替代方案

- 只在主脚本启动时检查 proof：拒绝；策略可在依赖安装子进程运行期间撤销。
- 让 VenvManager 保留无授权的便利入口：拒绝；它会绕开 Tools 的 execution proof 和 spawn permit。
- 持有策略读锁直到脚本或 pip 退出：拒绝；长任务会阻塞权限设置和撤销。
- 在 App 命令中只执行一次授权检查、不把 proof 传入 Tools：拒绝；owner 无法验证最终执行的 Skill 和输入。

## 影响与迁移

删除了内部无授权的 `VenvManager::ensure` 调用形式；调用方必须提供 `ProcessStartAuthorization`。Tauri 命令 payload、配置、数据库与持久事件没有改变，无需数据重置或配置迁移。测试版允许破坏性内部 API 调整，因此不保留旧入口。全局策略变化仍会取消其他 session 的 Skill；工具或 session trust 变化只会取消相应范围内的进程。

## 验证

- Skills crate 测试验证 setup 启动被拒绝时不会创建 child，以及 spawn permit 在 containment attach 后立即释放。
- Tools 测试验证 adapter 无 authorization context 时拒绝执行、proof context 中的过期策略在脚本读取前返回取消、Tools owner 仍在 owner 内检查 Skill 是否存在及启用。
- Windows Tools 回归测试验证策略撤销会终止 Skill 后代进程；Windows Skills 测试验证丢弃受管 setup wait 会关闭其进程树。
- `cargo fmt --all -- --check`、`cargo test --locked -p haven-skills`、Tools 目标回归、workspace tests/clippy 及 ADR 索引检查作为此跨 crate 切片门禁。

## 回滚

回滚时移除 App 直接 Skill 命令的 proof 传递、Tools port 的 proof 绑定、SkillRunner 撤销监听及 `ProcessStartAuthorization` 接口，恢复原有只在调用时授权的行为。这会重新允许策略撤销后 Skill/setup 进程继续运行，也会重新暴露无 proof 的内部启动入口。无持久数据需要恢复。
