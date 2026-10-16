# ADR 1255：监督 HCS guest workspace seed 与 Shell 生命周期

## 状态

平台内部 guest runner 与 mock 验证已实现；broker service/provider 尚未调用。配额 runtime gate 仍 fail closed，未创建真实 HCS system 或执行 guest 超额写入验收。

## 背景

现有组件分别构造闭合 HCS 配置、拥有 compute system 和 guest process handle、生成固定 workspace seed 参数、限制 stdout/stderr 并可导出已停止 writable layer，但缺少统一调用者来保证生命周期顺序。Shell 运行前必须先将只读输入副本 seed 到 quota-bound writable layer；执行期间需要同时 drain 两条输出 pipe；export 需要 HCS 确认 compute system 已停止的 proof。

## 决定

- 新增平台私有 `execute_guest` runner。调用方必须先绑定经验证的 writable layer 和只读 workspace input；runner 不创建 scratch、不声称 quota 已生效、不做 output export。
- runner 启动一个 compute system，先执行固定 workspace seed process。Seed stdout/stderr 全部 drain 但不进入用户输出；非零 seed exit code 立即终止此次运行，不启动用户 Shell。
- Seed 成功后，只创建一个由 typed request 构造的 `cmd.exe` 或 PowerShell 进程。受限进程使用固定应用、环境和受限 token；request command 只经过对应的固定 command-line builder，不允许调用方选择 guest executable、路径或环境 JSON 字段。用户 stdout/stderr 经现有 `WindowsContainerOutputSink` 逐流限制并继续 drain，超额字节被丢弃并标记 truncated。
- 进程监督按最多 25 ms 的有界 wait 轮询退出状态，并在每轮 drain 两条 pipe；只有 guest 进程已退出且两个输出 pipe 均关闭时才结束该进程阶段。request deadline 和取消会终止 guest process，并继续进入 compute-system stop 流程。
- 无论 guest seed/命令成功或失败，创建 system 后都显式调用 `terminate`。只有 HCS 返回成功才生成绑定原 writable-layer path 的 `HcsStoppedComputeSystem` proof。任何停止失败或 compute-system 创建结果不确定的错误都返回 `cleanup_safe=false`；调用方必须保留/隔离 attempt，不能清理层目录。创建 HCS 前的取消与 deadline 错误可安全清理。
- 成功结果携带 exit code、有界 captured output 和 stopped proof，供后续 output export/diff owner 使用。失败结果只携带固定错误类别，不暴露 OS 文本、命令或路径。
- native HCS API loader 仅在 broker 内部可调用。当前服务仍在 workspace staging 前因 quota 未证明而拒绝；本 runner 未接到 service/provider，不改变权限与网络策略。

## 替代方案

- Seed 与 Shell 共用一个 process：拒绝；用户命令可能观察或改变 workspace 初始化状态，且无法在 seed 失败时阻止 Shell 启动。
- 等待 guest process 后再读取 stdout/stderr：拒绝；pipe 写满可能让 guest 无法退出。
- 只依赖 `Drop` 终止 compute system 后创建 stopped proof：拒绝；best-effort teardown 不代表 HCS 已确认停止。

## 影响与迁移

增加 broker 内部 typed guest runner 与输出 sink trait，移除任意生产 layer path 构造入口仍由 ADR 1254 约束。IPC、数据库、配置与 public Tools API 不变；quota gate 和 backend unavailable 行为不变，无需重置。

## 验证

- mock HCS 测试覆盖 seed→Shell 顺序、有界输出回调、exit code、seed 失败不启动 Shell、取消及停止失败时 cleanup safety 分类。
- `cargo fmt --package haven-platform -- --check`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings` 与 ADR index checker。
- 未加载/调用 native HCS API，未创建真实 compute system 或执行 quota 验收。

## 回滚

移除 guest runner、输出 sink trait 与 native API内部 loader 接线即可回滚；当前 provider 尚未调用它，service 继续在 quota gate fail closed。
