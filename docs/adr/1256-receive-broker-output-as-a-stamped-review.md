# ADR 1256：将 broker 输出接收为带执行戳的 review

## 状态

Broker client 已实现成功响应消费和本地 review owner；service 仍在 quota gate fail closed，尚未发送 HCS 输出。未调用真实 HCS，也未执行 guest 超额写入验收。

## 背景

协议已定义 stdout/stderr 分块、输出差异元数据与文件块。发送端和本地差异重建逻辑已有独立实现，但 client 只接受 terminal control frame，无法把成功执行结果变成 Tools 已有的 `SandboxOutputReview`。如果 client 用可变 canonical workspace 作为差异基线，并发文件变化还可能让 review 与实际上传快照不一致。

## 决定

- 收到匹配 request ID 的 `WorkspaceSnapshotRequested` 后，client 先按 request 的 `workspace_copy` 上限复制一份私有 baseline，再从该 baseline 发送 workspace snapshot。后续 review 比较的基线与发送给 broker 的树相同。
- client 接收 stdout/stderr chunk 时按 request 中各自的字节上限累计；超限的 broker stream 视为协议错误。完成帧必须匹配 request ID，truncation 标记也必须符合 client 实际接收的长度。
- client 仅在匹配的输出差异流完成后创建成功结果：按 `output_diff` 从私有 baseline 重建输出树，复核 entry 顺序、路径、容量、chunk digest 与最终 manifest。broker 的失败响应不会生成 review。
- 成功后删除 baseline 与空的 file staging，只将重建后的输出 capability 留在 `SandboxOutputReview` owner 中。Owner 绑定 execution workspace generation、policy revision 与 initial snapshot bytes；创建文件晋升继续使用 `copy_reviewed_created_file_to`，并经现有 Tools 授权链决定是否执行。
- client staging 位于随机 TempDir，拷贝和协议传输均受 request 文件树上限、取消信号与单次 execution deadline 约束；执行命令不直接写入此目录。临时盘没有每实例 OS 硬配额，后续 broker provider 接线仍必须遵循 Tools pending-review 512 MiB 总预算，且不能把该预算当成 guest quota 的替代证明。
- 当前 server 不发送成功输出，唯一 quota runtime gate 继续位于 attempt 分配与 workspace 读取之前。此决定不连接 provider，也不改变生产执行可达性。

## 替代方案

- 直接把 canonical workspace 作为重建基线：拒绝；并发改写会让上传快照与 review 基线分离。
- 把 broker 返回的文件整体复制回项目根：拒绝；review 必须经过既有 manifest、stamp、确认和 capability 晋升路径。
- Client 成功时只返回 manifest、不持有私有输出：拒绝；后续审核/晋升需要保留已验证的文件内容。

## 影响与迁移

新增 broker client 内部 response DTO 与 TempDir-backed review owner；named-pipe v2 wire contract、数据库、配置和 Tools public API 不变，无需重置。每个请求产生的本地临时树受其 request limits 限制；取消或任何校验失败都会丢弃临时 owner。

## 验证

- named-pipe 端到端测试覆盖固定快照上传、stdout 分块、输出差异接收、文件晋升与 request-scoped failure。
- `cargo fmt --all --check`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings` 与 ADR index checker。
- 未调用真实 broker service/HCS；quota gate 未改变，未运行超额写入验收。

## 回滚

移除 client exchange 与 review owner，并恢复仅解析 terminal response 的旧 client 方法即可。回滚不改变 HCS quota gate；任何后续 provider 都必须保持 unavailable，直到输出接收能力重新实现。
