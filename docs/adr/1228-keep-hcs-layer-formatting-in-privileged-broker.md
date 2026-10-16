# ADR 1228：在具备 HCS 存储权限的 broker 中格式化 scratch layer

## 状态

平台 crate 已提供显式格式化端口；所需最小 OS privilege 尚未确定，HCS compute-system 与 guest quota 验收未通过。WorkspaceWrite 继续 fail closed。

## 背景

ADR 1227 实现了 capability 持有的每 attempt scratch 层目录、`sandbox.vhdx` 创建和 `MaximumSize` 复核。Windows 的 `HcsFormatWritableLayerVhd` 会在未挂载 VHD 上创建并格式化供 container writable layer 使用的分区；它是将磁盘变成 HCS layer 的下一步。

在当前 Windows 10 build 29683 的非管理员进程中调用该 API，得到 HRESULT `0x80070522`。该值对应 `ERROR_PRIVILEGE_NOT_HELD`（1314），说明普通桌面进程的 token 不具备此操作所需 privilege。官方 API 文档没有给出具体 privilege 清单，不能假设“管理员组成员”或某个已知磁盘 privilege 就足够。

## 决定

- `BoundedScratchVhd::format_for_container` 是显式、可失败的 OS 边界操作。格式化成功且重新查询确认 VHDX `VirtualSize` 未变化前，`layer_path()` 拒绝提供可传给 HCS 的 writable-layer 路径。
- 普通 Haven desktop process 不执行此操作。未来 HCS broker 必须拥有准确所需的 service token privilege，并在安装/启动阶段验证 capability；权限缺失或 HCS DLL/入口不可用时保持 fail closed。
- scratch VHD 创建仍可在普通进程中完成并受精确的虚拟最大容量约束；格式化错误时 owner 保留资源供显式 cleanup 或受控重试，不能静默释放宿主容量预留。
- HCS 格式化、`HcsInitializeWritableLayer`/layer metadata、compute-system 启停、终止后的资源回收均由 broker lifecycle owner 管理。需在支持目标上先确认最小 privilege，再证明 guest 超额写入得到容量错误和 scratch 清理成功。
- 不把本地格式化错误解释成 HCS quota 成功或失败；在 runtime probe 通过前，Shell/Skill/MCP WorkspaceWrite 保持拒绝 opaque execution。

## 替代方案

- 让 Haven 主进程直接请求提升：拒绝；这会把容器管理权限放进拥有 UI、凭据和其他桌面能力的进程。
- 在代码中预设 `SeManageVolumePrivilege` 一定满足 HCS 格式化：拒绝；本次失败只证明当前 token 缺少某个 OS privilege，并未识别具体 privilege。
- 格式化失败后仍把目录加入 HCS `Layers`：拒绝；该目录不满足 writable-layer 的格式要求，不能传给 compute system。

## 影响与迁移

增加一个未连接工具的 HCS 格式化端口，不增加 IPC/service installer、配置或权限 UI。普通 Haven 执行不会触发它。当前 HCS 格式化回归 probe 标记为 ignored，待具备经验证的 broker token 后运行；被忽略状态不是通过证据。

## 验证

- Microsoft [`HcsFormatWritableLayerVhd`](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsformatwritablelayervhd) 说明 API 接受未挂载 VHD 并创建/格式化容器 writable layer 分区。
- Microsoft [Windows system error 1300–1699](https://learn.microsoft.com/en-us/windows/win32/debug/system-error-codes--1300-1699-) 将 1314/0x522 定义为 `ERROR_PRIVILEGE_NOT_HELD`。
- 当前宿主上的 `cargo test --locked -p haven-platform`：VHDX 创建、最大虚拟容量查询和 capability cleanup 通过；需要 HCS privilege 的格式化 probe 被忽略。手动非管理员调用返回 `0x80070522`。`cargo clippy --locked -p haven-platform -- -D warnings` 通过。

## 回滚

若目标 Windows 无法以可接受的 broker privilege 安全格式化并使用 scratch layer，则移除该 formatter，并关闭可选 HCS backend；保留当前 AppContainer 路径和 WorkspaceWrite fail-closed 策略。
