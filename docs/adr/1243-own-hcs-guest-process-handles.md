# ADR 1243：由 HCS process owner 管理 guest 进程与 I/O handles

## 状态

已实现 crate 私有 HCS process owner 和 native API adapter。它依赖 ADR 1241 的 started compute-system owner 与 ADR 1242 的 typed `ProcessParameters`，但 broker service 尚未调用；stdout/stderr 目前仅能转移为受管 handle，还没有 reader、输出 sink 或 review 重建。quota gate 保持 fail closed，未创建真实 guest process。

## 背景

ADR 1242 已能从已校验的 Shell request 构造闭合 HCS `ProcessParameters`，但缺少 `HcsCreateProcess` 的 operation result、进程退出轮询、终止与句柄清理实现。Windows HCS 返回 HCS process handle 和可选 stdio handles；operation/result 文档与 process exit status 由 HCS 分配并要求调用方释放。如果这些对象没有统一 owner，创建超时、部分初始化、取消或调用方提前退出都可能遗留进程或句柄。

## 决定

- `HcsComputeSystemOwner` 只在 system 已 start 且仍持有 system handle 时创建 guest process。借用关系保证 guest process owner 存活期间 compute-system owner 不可终止或释放。
- 创建调用使用固定 `HcsGuestProcessParameters` JSON、空 security descriptor 和有界 operation deadline；等待 `HcsWaitForOperationResultAndProcessInfo` 同时取得 PID、process handle 与 stdio handles。创建失败、超时或返回不完整信息时，取消/排空 operation，并尽力终止进程、关闭已返回 handles。
- guest process owner 分别拥有 HCS process handle 与 stdout/stderr Win32 handles；可将 stdout/stderr 转交给另一个 RAII handle owner。销毁仍在运行的 process 时先有界终止，再关闭未转交的 pipes 与 process handle。显式终止失败时保留 owner 状态，使 Drop 再做一次 best-effort 尝试。
- `HcsWaitForProcessExit` 只接受大于零且不超过既有 operation 上限的等待时长。HCS timeout 映射成 pending；成功返回的 `ProcessStatus` 仅保留匹配的 PID、有效退出码与成功 `LastWaitResult`。JSON 分配上限与 UTF-16 allocation 边界都检查，HCS 返回字符串通过 `LocalFree` 释放。
- HCS adapter 从 System32 的 `computecore.dll` 加载 `HcsCreateProcess`、operation/process wait、terminate/close API。原始 HCS 错误 JSON 不进入日志或命令错误。
- 该 owner 尚不读取输出，不运行进程树撤销 watcher，不创建 writable layer，也不把进程接到 service、provider、Tools 或用户输入。生产 backend health 与 `WorkspaceWrite` quota gate 不变。

## 替代方案

- 在 service handler 里直接调用 HCS API 并散落管理 handles：拒绝，超时和部分初始化的资源回收容易遗漏。
- 以无界等待读取 process exit：拒绝，broker shutdown 与执行 deadline 不能被 HCS 阻塞等待绕过。
- 把 HCS result JSON 或裸 stdio HANDLE 暴露给上层：拒绝，扩大不稳定协议和资源所有权边界。
- 仅关闭 process handle，不终止仍运行的 process：拒绝，system 生命周期与 guest process 生命周期不能安全脱钩。

## 影响与迁移

新增 Windows-only 私有模块和 `windows-sys` `Win32_System_Memory` feature，用于安全读取 HCS 分配的有界本地字符串。没有数据库、配置、named-pipe、Tauri IPC 或持久状态变化，不要求重置数据。HCS process exports 缺失或主机不支持时，该 HCS runtime 初始化失败；外部 broker quota gate 仍返回 `quota_unattested`。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- mock 单测覆盖未启动时拒绝创建、创建到退出的 compute/process 生命周期、pending 与退出解析、PID/状态异常、显式终止失败重试、stdout/stderr 转交与 drop 清理顺序。
- 本次未执行真实 HCS compute system/process、Windows Container、broker 服务、guest quota 超额写入或输出管道压力验收；这些环境验收按用户要求暂缓，quota runtime attestation 仍是生产执行硬门槛。

## 回滚

移除私有 guest process owner、Native HCS process API adapter、对应测试与本 ADR/路线图引用即可回滚。该代码没有进入 broker 请求路径，不需要清理持久状态；不得因此把用户 command 或 HCS 原始句柄直接交给 service 上层。
