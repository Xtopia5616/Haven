# ADR 1260：在 broker 启动时证明 HCS guest storage quota

## 状态

实现已接入 broker 启动与 Shell 请求 gate。真实 HCS 执行、超额写入和目标 Windows build/镜像兼容性验收未运行；这些仍是开放的环境验收 Gate。

## 背景

HCS JSON 的 `StorageSandboxSize`、VHDX 的 `MaximumSize` 和格式化成功都不能单独说明目标 guest 的实际写入会在该容量处失败。ADR 1231 将 runtime quota attestation 定为生产执行的前置条件，并要求证明失败时，在创建执行 attempt 和接收 workspace 数据前拒绝请求。

## 决定

- `WORKSPACE_WRITE_STORAGE_QUOTA_BYTES` 是 Tools 与平台共用的 512 MiB 固定值。当前 attestation 只授权该精确值；named-pipe 请求不能自行选择新的 quota。
- broker 服务在进入 `SERVICE_RUNNING` 前，对本机 pinned image catalog 和精确 quota 创建一个私有 attempt/VHDX，并运行固定 PowerShell/.NET probe。probe 不执行模型命令、不运行 workspace seed、不映射用户 workspace；它向 guest `C:\Windows\Temp` 用随机数据顺序写入 1 MiB 块，最多尝试 `quota + 1 MiB`。只有 guest 返回 Windows `ERROR_HANDLE_DISK_FULL`（39）或 `ERROR_DISK_FULL`（112）时脚本才输出固定 UTF-8 marker 并以 0 退出。
- 接受证明还要求 probe 完成写入量至少为 quota 的一半、同一 VHDX 物理文件增长至少为 quota 的一半、guest 停止证明绑定该 writable layer、停止后 VHDX 最大虚拟容量仍与 quota 相等、stdout marker 精确且没有 stderr/截断，并且宿主卷在写入前有 `quota + 1 GiB` 可用余量、写入后仍有至少 1 GiB 可用余量。任一条件不满足即不签发证明。
- runtime proof 是不可序列化的进程内对象，持有同一 `NativeHcsApi` 和 pinned image catalog，并绑定精确 512 MiB quota。只有证明存在且请求 quota 精确匹配时，broker 才在 attempt 分配和 workspace snapshot 前继续；否则返回 `quota_unattested`。后续 Shell 执行复用该 API 和镜像目录，不会按请求重新加载可变选择。
- 服务 probe 失败时仍可报告 SCM `SERVICE_RUNNING`，但这不代表 backend ready；服务继续认证协议请求并在 workspace 读取前 fail closed。服务启动等待上限调整为 5 分钟，以覆盖 probe 的 4 分钟 deadline 和停止清理时间。
- SCM startup-pending 接受 Stop/Shutdown；服务取消事件传到 probe 的 HCS process loop，确认 guest/HCS 停止后清理 attempt，再结束启动。若停止无法证明，attempt 仍进入 recovery quarantine。
- 已确认 HCS 停止后按 owner 顺序清理 VHDX 与 attempt；停止或 quiescence 不确定时写入 recovery marker 并隔离 attempt，不自动回收。
- `broker_service.rs` 当前超过 800 行。保留其 SCM 安装、状态、控制回调和 listener 生命周期在同一 Windows service owner 内；quota probe/evidence/attempt cleanup 已独立放入 `quota_attestation.rs`，继续切分会扩大私有 SCM 状态跨模块面。后续若出现独立消费者，再复核边界。

## 替代方案

- 仅相信 HCS 配置字段或 VHD 元数据：不能证明 guest 写入确实遇到限制，拒绝。
- 每个 Shell 请求前都写满 quota：会重复执行最多 512 MiB 的启动工作；证明只依赖进程启动时使用的 pinned image、HCS API 与固定容量，因此每个服务进程探测一次。
- 对所有客户端请求 quota 做缓存：会让未实际测量的容量获得生产授权，拒绝。
- Probe 失败后退回 host Shell 或跳过 workspace gate：会绕开 WorkspaceWrite 边界，拒绝。

## 影响与迁移

无数据库、持久配置、IPC wire 或用户数据变化，无需重置。固定 quota 从 Tools 局部常量收敛为 platform execution contract 常量。Windows broker 启动最多等待 5 分钟；HCS/镜像/服务 token 不适配或宿主卷余量不足时，服务仍可运行协议但 WorkspaceWrite Shell 返回 quota 未验证。Skills、MCP 和其它 opaque capability 仍按既有规则拒绝。

## 验证

- 自动测试覆盖固定 probe 参数与 quota、单进程 probe 生命周期、缺失/意外 marker、写入计数边界、VHDX 增长、宿主空间 reserve、stdout/stderr 完整性和无证明请求 gate。
- 已通过 `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`，以及本切片最终变更后的 `cargo test --locked -p haven-platform`（164 passed、1 ignored）和 `cargo clippy --locked -p haven-platform -- -D warnings`。
- 已通过 `corepack pnpm --dir ui run check`、`corepack pnpm --dir ui run test:run`（1318 passed）和 `corepack pnpm --dir ui run build`，以及 `scripts/check-adr-index.ps1`、`scripts/check-crate-dependencies.ps1`。
- 尚未运行真实 HCS guest、quota overrun、目标 Windows build/API、broker privilege token 或镜像兼容性验收；这些验证依赖受支持的 Windows Container/HCS 测试环境，按用户要求本轮不安排或运行。

## 回滚

回滚时删除启动 probe 与 attestation proof，并恢复请求始终返回 `quota_unattested` 的固定 gate。不得退回仅依据 `StorageSandboxSize` 或 VHD 元数据放行，也不得恢复 host Shell fallback。
