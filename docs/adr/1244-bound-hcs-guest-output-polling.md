# ADR 1244：限制 HCS guest stdout/stderr 的轮询读取

## 状态

已实现 crate 私有输出读取 owner，复用 ADR 1243 的 stdout/stderr handle owner 和 ADR 1225 的 per-stream output sink。每次 poll 对每条流最多消费 64 KiB，单次 `ReadFile` 不超过 16 KiB；provider/service 尚未调用它，也没有 HCS process 或 writable output layer 接线。quota gate 保持 fail closed。

## 背景

ADR 1243 已取得并能转移 HCS 返回的 stdout/stderr handles，但尚未把 pipe 字节交给 `WindowsContainerOutputSink`。如果先阻塞读取 stdout 再读取 stderr，任一输出管道写满都可能卡住 guest shell；若把完整管道内容积存在内存，也会绕过既有 output ceiling。微软 `PeekNamedPipe` 支持匿名管道读取端并能报告当前可读字节数；微软同时指出，同步 handle 上存在并发 I/O 时该调用也可能阻塞。因此读取器必须独占两条 handles，并逐次做有界读取。

## 决定

- 新增私有 `HcsGuestProcessOutputReader`，从 process owner 一次性取得 stdout/stderr 各自唯一的 handle 所有权；重复转交或缺失任一 handle 均失败。
- poll 按 stdout、stderr 顺序分别检查可用字节，每条流最多读 64 KiB；每次 `ReadFile` 上限 16 KiB，且仅在 `PeekNamedPipe` 报告该数量可读后调用。pipe 已关闭时释放对应 handle 并报告 closed；其它 Win32 错误直接失败，不伪装成 EOF。
- 每个字节块立即交给现有 `WindowsContainerOutputSink`。sink 保留/发布部分受每流上限约束；即使已截断，读取器仍继续消费实际 pipe 字节，避免输出上限之后 guest 因管道写满阻塞。
- 句柄只由 output reader 使用；同一 handle 不允许并发 peek/read 或另行复制后读取。HCS provider 仍须从外层检查 cancel/deadline、观察进程/compute-system 生命周期，并在结束或撤权时关闭整个 compute system；本读取器不生成线程、不拥有 deadline、不代替 process-tree 清理。
- 当前 guest `WorkingDirectory` 仍在只读 input，且没有可写层/输出树导出路径，所以此能力不启动真实 command，也不连接 service 或 Tools。

## 替代方案

- 先同步阻塞读取 stdout，再读取 stderr：拒绝，另一条管道可能写满并使 guest 与 reader 相互等待。
- 把 pipe 全部读入无界 `Vec` 后再截断：拒绝，内存占用不受 execution output limits 限制。
- 输出达到 sink 上限后停止读取：拒绝，guest 仍可能阻塞在 pipe 写入。
- 在读取器中私自派生 reader 线程并依赖 Drop 无界 join：拒绝，线程与 process/system 取消必须由未来 provider 统一编排。

## 影响与迁移

新增 Windows-only 私有输出读取模块，无数据库、配置、IPC、权限 key 或持久化变更，不需要重置。执行端口返回的 stdout/stderr 类型与上限不变。HCS process output handles 缺失、不能 peek/read 或状态不确定时，未来 provider 应终止该 attempt 并 fail closed。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- Windows 匿名 pipe 单测覆盖两流独立读取、关闭识别和 sink 截断后的继续 drain。
- 这些测试未启动 HCS；未执行 HCS pipe 句柄、compute-system 撤销、guest descendant、实时 callback 压力或 quota 的真实 Windows 验收。真实 HCS quota attestation 仍为生产执行硬门槛。
- API 契约参考 Microsoft [HCS_PROCESS_INFORMATION](https://learn.microsoft.com/en-us/virtualization/api/hcs/Reference/HCS_PROCESS_INFORMATION)、[PeekNamedPipe](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-peeknamedpipe) 与 [ReadFile](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile)。

## 回滚

移除私有 output reader、对应测试及本 ADR/路线图引用即可回滚。该模块没有接入执行路径，不影响持久状态；不得以无界 stdout/stderr 缓冲替代。
