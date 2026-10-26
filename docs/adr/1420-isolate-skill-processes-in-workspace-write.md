# ADR 1420：在 WorkspaceWrite 下隔离 Skill 子进程

## 状态

已实施；策略编译和 Skill 回归测试通过。为规避用户 LocalAppData 下 Python Install Manager runtime 在 MXC 中的 `STATUS_ACCESS_DENIED`，Windows 现在优先选择 Program Files 下的机器级 Python，再尝试其它 PATH runtime 与用户级 runtime。真实 MXC 端到端验收仍待完成：本机没有机器级 Python，因此无法验证该候选在 MXC 下创建 venv；没有可用机器级 runtime 时，用户级候选仍会按顺序尝试。此路径没有 host-process fallback。

## 背景

Skill 激活、venv 创建、requirements 安装和 Skill 脚本执行是不同阶段。之前 setup 与脚本仅使用 Job Object 管理进程树；WorkspaceWrite 拒绝 Skill 启动，FullAccess 下依当前用户权限运行。把 setup、pip 和脚本都视作同一种进程会错误地授予项目写权限或网络访问。

## 决定

- Skill 进程启动请求携带用途、Skill 根、venv 根、调用 scratch 和运行时只读依赖。Tools 的实时授权 owner 在每次 spawn 前按该请求生成 MXC 策略；Skills 只消费 opaque permit，不编译路径或网络权限。
- WorkspaceWrite 下仅 Windows MXC 启动 Skill 子进程；MXC 不可用、策略不能编译或 probe 失败时拒绝启动，不回退到宿主进程。ReadOnly 拒绝 Skill 子进程。FullAccess 保留宿主进程边界与现有进程树管理。
- venv 创建离线执行，只写 app-owned venv 目录与单次 setup scratch，Skill 源文件和 Python/runtime 依赖只读；不把 workspace 写根授予 setup 进程。
- `pip install -r requirements.txt` 是独立的 opaque network 操作。只有全局网络为 Open、Skill 工具级网络未设为 Deny/Restricted、且全局与 Skill 都没有域名 allowlist 时才允许公网出站。MXC 不授予私网或宿主 loopback；域名约束存在时拒绝，而不近似实现白名单。
- Skill 脚本的 WorkspaceWrite 策略继承当前执行工作区、全局与 Skill 工具级文件路径交集、deny/read-only 规则，并额外只写单次 scratch。Skill 源、venv、脚本快照和实际 Python runtime 作为只读依赖；脚本网络离线。Read-only 工具资源策略仍移除 workspace 写权限。
- setup 命令仅在 MXC 分支把 cwd 与 `TEMP`、`TMP`、`TMPDIR` 指向受控 scratch；FullAccess 保持原有宿主环境。脚本 cancellation、超时和授权撤销会终止子进程并保留 containment owner，直到进程退出。
- MXC 创建进程前只解析可直接启动的 Python 可执行文件绝对路径，不启动未隔离的 Python 探测进程；跳过 WindowsApps 执行别名，优先搜索 Program Files 下的机器级 Python，再考虑其它 PATH runtime 和用户级 Python Install Manager runtime。进程自身仍由每次 setup authorization 编译并启动于 MXC；runtime 依赖保持只读。

## 替代方案

- 仅挂 Job Object：拒绝，它限制进程树生命周期，不限制文件和网络访问。
- setup 复用整个 workspace 写根：拒绝，venv/pip 不需要修改项目文件。
- 对 pip 使用 MXC Open 且保留域名列表：拒绝，MXC 网络策略无法强制域名范围。
- 为 Skill 脚本默认开放网络：拒绝，WorkspaceWrite 的 Skill contract 不请求网络；需要网络的包安装是单独用途。
- 以宿主进程作为 MXC 不可用时的 fallback：拒绝，会静默扩大权限。

## 影响与迁移

WorkspaceWrite 下 Windows 现在可以运行 Skill，但只有脚本获得现有工作区写策略；setup 写入限制到应用私有环境与 scratch。pip 首次安装可能因网络被拒绝，需要 Open 且无域名约束。非 Windows 的 WorkspaceWrite Skill 仍拒绝。配置、IPC 和数据库 schema 不变；ToolRun 通过目标 Skill 的同一授权与隔离入口执行。

## 模块边界

`security/execution.rs` 超过项目的 800 行复核线，但保留为所有进程入口共享的短期 process-start permit 与撤权 owner；每种进程策略的文件/网络根编译已放入 `security/execution/process_policy.rs`。将 permit owner 再按 Skill、Hook、MCP 等进程种类拆开，会分散同一 revision guard 和运行中撤权语义，因此本切片不引入第二个状态 owner。`skill_runner.rs` 也超过复核线；它仍是单次 Skill 调用的唯一生命周期 owner，包含脚本快照、scratch、取消/撤权、子进程与有界输出，venv/setup 已归 `haven-skills`。后续只有出现独立生命周期消费者或这些状态再次分裂时才拆分。

## 验证

- `cargo check --locked -p haven-tools` 通过。
- `cargo test --locked -p haven-tools --lib skill_ -- --test-threads=1`：29 passed；覆盖 Skill authorization、setup 私有写根、pip 网络门槛、取消/撤权、工作区临时目录和输出预算。
- 实际 Skill MXC 用例 `workspace_write_skill_setup_and_script_obey_mxc_file_policy` 曾在 Windows 执行但失败：Python Install Manager runtime 位于用户 LocalAppData，MXC 报告 `STATUS_ACCESS_DENIED`。当前候选顺序的单元测试覆盖机器级 runtime 优先；本机未安装 Program Files Python，真实 MXC 验收需在有机器级 Python 的 Windows 环境重新运行。本 ADR 不把此前失败或候选顺序测试记作端到端通过。

## 回滚

可恢复 WorkspaceWrite 下拒绝 Skill 子进程；不得移除每次 spawn 的实时授权、取消/撤权或 FullAccess 行为。无需重置持久数据。
