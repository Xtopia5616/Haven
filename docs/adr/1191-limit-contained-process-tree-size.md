# ADR 1191：限制受管进程树的活动进程数

## 状态

已采纳并实施。

## 背景

Windows `ProcessContainment` 已用 Job Object 在取消、断开和应用退出时回收 Shell、Skill、Skill bootstrap 与 MCP stdio 的子进程树，但没有限制单棵树的活动进程数。恶意或失控命令可以持续创建后代进程，消耗系统进程资源。

## 决定

- 每个 Windows Job Object 同时启用 `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 与 `JOB_OBJECT_LIMIT_ACTIVE_PROCESS`，活动进程上限固定为 256，计数包含启动的根进程及其子孙。
- Job Object 在子进程第一条指令执行前完成配置和分配；达到上限后，进程树内创建的后续进程会失败。无法创建或配置 Job Object 时，启动仍 fail closed。
- 该上限应用于所有通过 `ProcessContainment` 启动的子进程，含 Shell 前台/后台、Skill runner/bootstrap 与 MCP stdio。
- 固定 256 为 Cargo/常见构建工具和多进程 MCP server 留出运行空间，同时为单次受管调用设置可解释的进程数上限；设置页完整权限说明此限制。
- 此限制只界定受管 Job 内同时活动的进程数，不限制 CPU、内存、文件系统或网络访问，也不提供 AppContainer 隔离。对进程外系统 broker 的请求不由此限额约束。

## 替代方案

- 只在 Shell 工具结果或并发槽中限流：拒绝，子进程树可以脱离工具调用继续创建后代。
- 降低到几十个进程：拒绝，可能阻断 Cargo、测试 runner 或复杂 MCP server 的正常并发工作。
- 用 Job Object 代替文件/网络 sandbox：拒绝，进程计数与资源授权是不同边界。

## 影响与迁移

一棵受管进程树达到 256 个活动进程后，新子进程创建会失败。该限制仅作用于 Haven 管理的子进程，不修改全局系统限制；无配置、IPC、数据库或重置变化。

## 验证

- Windows adapter 测试把 Job 限额设为 1，启动唯一根进程后验证其子进程创建被 Windows 拒绝；另有正常后代回收覆盖。
- 运行 workspace Rust 测试/check/严格 Clippy、UI 检查与测试、格式检查、ADR 索引、crate dependency inventory 和 `git diff --check`。

## 参考

- [Microsoft：Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
- [Microsoft：JOBOBJECT_BASIC_LIMIT_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)
