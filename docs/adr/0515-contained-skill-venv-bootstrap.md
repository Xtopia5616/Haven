# ADR 0515：Skill 虚拟环境准备进程使用 Windows containment

## 状态

已采纳，实施完成（2026-10-05）。

## 背景

ADR 0513 已让 MCP stdio、Skill script、Shell 和后台 Action 在 Windows 上先挂起进程、加入 kill-on-close Job，再恢复初始线程。源码复核发现 `haven-skills::VenvManager::ensure` 仍直接启动两个环境准备进程：`python -m venv` 和 `python -m pip install -r`。这些进程可能在加入 Job 前派生子进程，导致调用方结束时留下进程树。

Skill script 本体由 `haven-tools::SkillRunner` 执行并已受 containment 管理。虚拟环境属于 `VenvManager` 的准备职责，因此由该 owner 在两个启动点复用 `haven-platform::ProcessContainment`。`haven-skills` 当前只依赖 `haven-common`；新增的 `haven-skills → haven-platform` 是单向下层能力边，platform 不依赖 Skills。

## 决定

1. `VenvManager::ensure` 的 venv 创建和 pip 安装共用一个私有启动 helper：创建 `ProcessContainment`，配置 suspended-create，spawn 后立即按 pid 与原始句柄 attach/resume，再 `wait_with_output`。非 Windows 继续走 platform 的 no-op containment。
2. helper 显式配置 stdin 为 null、stdout/stderr 为 piped，保留原 `Command::output()` 的诊断行为；保留工作目录、命令参数、继承的进程环境、默认 creation flags、错误上下文和 `encoding::decode_lossy`，不使用 Skill script 的 `env_clear()` 配置。非零状态继续由各调用点产生原有 venv/pip 错误。helper 不接管 Tokio child 生命周期；`kill_on_drop` 与进程树 Job guard 在等待期间共同存活，future 被丢弃时回收子进程及后代。
3. requirements fingerprint 仍只在 pip 成功后写入；pip spawn、attach、wait 或非零退出都不写 marker，下一次 `ensure` 会重新尝试依赖安装。已有 fingerprint 命中与 requirements 变更行为保持不变。
4. 新增 `haven-platform` 直接依赖，并更新唯一权威依赖表/图。不给 `haven-platform` 增加 Tokio 依赖，不在 `haven-tools` 复制 venv 细节，也不新增适配 port。
5. 本切片不调整并发 `ensure` 锁，也不改变 `SkillRunner` 的取消/timeout API：目前其显式 token/timeout 仲裁在 venv ensure 之后，本变更不延伸该生命周期；若 ensure future 被丢弃，Job guard 必须回收其进程树。
6. Job Object 只提供生命周期回收，不构成 Python 的文件、网络或权限沙箱。

本切片不改变 `python.exists()` 作为 venv 已创建判据。若 venv 创建失败后留下 Python 路径但目录内容不完整，后续调用仍可能跳过 venv 创建；修复半成品检测或原子创建属于单独生命周期问题，不作为本 ADR 的重试保证。

## 替代方案

- 继续直接用 `.output()`：拒绝。venv/pip 能在首次执行前派生未受 Job 管理的后代。
- 把 venv provisioning 移到 `haven-tools`：拒绝。会把 Skills 的环境/requirements/fingerprint 规则从 `VenvManager` owner 拆开，并重复平台进程启动适配。
- 通过公共 trait/闭包把进程启动器从 `haven-tools` 注入 `haven-skills`：拒绝。当前唯一消费者是 Tools，稳定 OS API 已由下层 `haven-platform` 提供；注入层会增加契约而不减少依赖或重复逻辑。
- 让 `haven-platform` 直接启动并等待 Tokio child：拒绝。它会接管 async child、stdio 和调用方生命周期，违反 ADR 0513 的 adapter ownership。

## 影响与验证

- 只增加一个单向 crate 依赖和私有进程 helper；不改 UI、IPC、schema、配置或持久数据，无需重置。
- 测试 setup 命令参数和 working directory、成功命令 stdout/stderr、非零退出诊断、失败 pip 不写 fingerprint 且可重试；Windows 测试覆盖丢弃等待中的 ensure helper 后，child 与 descendant 都被 Job 回收。嵌套 Job 分配失败时必须 fail closed，不退回未受管 spawn。ADR 0513 的 suspended-create/attach 失败关闭测试继续覆盖 platform 句柄与恢复失败。
- 适用门禁：格式化、`cargo check --workspace --locked`、`cargo test --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`scripts/check-crate-dependencies.ps1`、ADR index 与 `git diff --check`。不涉及 IPC/UI。新增 crate 边后执行 workspace crate-dependency 检查；Windows 进程树行为在当前 Windows 环境验收。安装包/UI 发布验收仍是 ADR 0395 的独立 Gate。
- 实际验收（2026-10-05）：`cargo fmt --all -- --check`、workspace check/test/strict Clippy、crate dependency 检查、ADR index 检查和 `git diff --check` 均通过；Skills venv 测试覆盖命令配置、输出/失败诊断、pip 失败重试与 fingerprint，以及 Windows 等待 future 取消后的 descendant 回收。IPC/UI 门禁不适用；安装包/UI 发布验收仍由 ADR 0395 独立跟踪。

## 回滚

删除 `haven-skills → haven-platform` 依赖与 helper，恢复 `VenvManager::ensure` 的两个 `.output()` 启动点，并还原 architecture dependency inventory；不涉及数据库、配置或用户数据。回滚会重新打开 venv/pip child 在 Job 分配前派生后代的窗口。
