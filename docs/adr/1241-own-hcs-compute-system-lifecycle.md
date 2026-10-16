# ADR 1241：由 broker owner 管理 HCS compute-system 生命周期

## 状态

已实现私有 HCS API 装载、异步操作等待与 compute-system RAII owner，并通过 mock 生命周期单测。该 owner 尚未接入 broker 请求处理；quota attestation 仍固定 fail closed，未执行真实 HCS 创建、启动或 guest 超额写入验收。

## 背景

ADR 1240 已把有界 workspace snapshot 传到 broker 私有 staging，但完整传输后仍返回 `backend_unavailable`。固定 HCS JSON 构造器已经收口了 isolation、镜像层、存储 ceiling 和只读 workspace 映射，却没有负责 HCS 句柄的创建、启动、终止和释放。直接把不匹配的 FFI 调用散落在 broker request handler 会使超时、失败回滚与 handle 生命周期难以统一审查。

## 决定

- 增加 crate 私有 `HcsComputeSystemOwner`。它只接受 `WindowsContainerSandboxConfig`，HCS ID 来自该配置的受校验名称，JSON 只能由闭合 builder 生成；不接受调用者提供的 HCS JSON、宿主路径或 isolation 参数。
- Native API 实现从 `System32\computecore.dll` 动态解析所需函数，避免普通 Haven 启动或不支持 HCS 的机器因为静态导入而依赖容器运行时。模块装载使用 `LOAD_LIBRARY_SEARCH_SYSTEM32`。
- 每次 create/start/terminate 都通过独立 HCS operation 执行并以 `HcsWaitForOperationResult` 有界等待；调用契约见 Microsoft 的 [create](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcscreatecomputesystem)、[start](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsstartcomputesystem)、[terminate](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsterminatecomputesystem) 和 [wait](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcswaitforoperationresult) API 文档。operation、compute-system 和 HCS 返回的 result document 各有本地 owner；result document 使用 `LocalFree` 释放，内容不进入日志或错误文本。
- 创建成功后 owner 才能 start；重复 start 被拒绝。显式 terminate 成功后关闭 compute-system handle；terminate 失败时保留 handle，允许调用方重试。owner drop 时尽力 terminate，再关闭 handle；固定配置的 `TerminateOnLastHandleClosed` 作为最后的生命周期回收机制。
- HCS 操作失败、超时或取消后，错误路径尽力取消并有界 drain operation。create 若已得到部分 compute-system handle，则在返回错误前尽力 terminate 并关闭它。
- 该 owner 仍未接入 service。quota gate 固定返回 `quota_unattested`；不会因生命周期实现而创建 compute system、运行 guest 或开启 `WorkspaceWrite`。后续 provider 还需实现 guest 进程、stdout/stderr、受限输出流、review 重建和 Tools 调用链。

## 替代方案

- 在 service handler 里直接调用 HCS FFI 并手动关闭句柄：拒绝，错误路径和取消时容易漏掉终止或释放。
- 静态链接 `computecore.dll`：拒绝，HCS 是可选后端，缺少对应系统组件时不应影响主应用加载。
- 把 HCS result JSON 写入 broker 错误或日志：拒绝，provider 返回内容不需要跨越控制面边界。
- 在该切片中解除 quota gate 并做一次“能启动即通过”的 HCS 探测：拒绝，创建成功并不能证明运行期写入 ceiling 生效。

## 影响与迁移

新增仅供 Windows broker/provider 后续使用的私有模块，不改变 named-pipe 协议、数据库、用户配置或 Tauri IPC。协议 v2 与当前 broker 产物无需因本切片更改。没有旧持久数据需要迁移。

## 验证

- `cargo fmt --package haven-platform -- --check`
- `cargo check --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `cargo test --locked -p haven-platform`
- mock 测试覆盖 create/start/terminate/close 顺序、start 失败后的 drop 清理、显式 terminate 失败后的重试，以及操作等待时间边界。
- Native FFI 只进行编译检查，未调用 HCS；未安装或启动 broker 服务，未创建真实 compute system、运行 guest、验证存储配额或执行超额写入测试。

## 回滚

移除私有 HCS lifecycle 模块和本 ADR 路线图引用即可回滚；broker 当前仍在 quota gate 前拒绝请求，不产生需要清理的 HCS 或持久状态。不得以回滚为由增加静态 HCS 依赖或放开 `WorkspaceWrite`。
