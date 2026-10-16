# ADR 1249：导出已停止的 HCS writable layer

## 状态

已实现私有 `HcsExportLayer` adapter、输入校验和 mock 链路测试；未调用真实 ComputeStorage API，未连接 broker service/provider，也未验证实际 HCS transport 格式。quota gate 继续 fail closed。

## 背景

ADR 1248 增加了解码 legacy `Files`/Windows backup-stream transport 的有界 extractor，但没有取得 HCS writable layer transport 的步骤。Microsoft 的 `HcsExportLayer` API 把 layer 导出到指定目录，并接收描述 parent layers 的 JSON 和 export options；hcsshim 的 ComputeStorage 封装将 export 目录作为预先存在的目录，并用 `IsWritableLayer` 标记 writable layer。

当前 broker 的 writable layer 位于每 attempt scratch folder，父层路径来自 broker 的闭合 HCS config。导出结果只能进入 attempt-private transport capability，再由 ADR 1248 解码到独立的 workspace output capability。

## 决定

- 新增 crate-private adapter，通过 `LOAD_LIBRARY_SEARCH_SYSTEM32` 加载 `computestorage.dll` 并解析 `HcsExportLayer`；缺少 DLL/入口或 HRESULT 失败时立即 fail closed。
- 导出计划只从已绑定的 writable layer folder 和 config 中的 parent layers 构造。计划要求 `sandbox.vhdx` 是非空普通文件、parent layer ID 唯一、transport/output 目录为空，并要求 writable layer、父层、transport 和 decoded output roots 两两不重叠。
- `LayerData` 只包含 config 中的 parent layer ID/path，并按 ComputeStorage schema 序列化为 `Id`/`Path`（HCS v1 container document 的 `ID` 字段不能直接复用）；`ExportLayerOptions.IsWritableLayer` 固定为 `true`。导出只允许在 compute system 已停止并释放 writable layer 后执行；任何错误或取消都要求 owner 丢弃整个 attempt。
- `HcsComputeSystemOwner::terminate` 仅在 HCS 明确返回终止成功后生成 `HcsStoppedComputeSystem` proof，并绑定原 writable-layer path。导出计划必须收到匹配该 path 的 proof；`Drop` 的 best-effort 清理不能伪造停止成功。
- 成功导出后必须继续交给 ADR 1248 extractor。当前 extractor 只接受已知 legacy `Files` transport；不存在该布局或记录无法解析时失败，不回退为直接复制 layer folder 或映射可写 host path。
- 原生 `HcsExportLayer` 是同步调用，没有 cancellation 或 deadline 参数。当前仅提供私有 adapter，不能从 broker listener/request loop 直接调用；在接线前必须有隔离 worker owner 管理同步调用生命周期、deadline、attempt 清理和 transport staging 预算。
- 该 API 文档没有承诺 transport 的具体目录布局。本地 mock fixture 不证明目标 Windows build 的导出格式；必须在受支持 HCS 主机确认 `Files` 布局兼容后才能接入生产 provider。quota runtime attestation 与真实 HCS guest 超额写入门槛保持不变。

## 替代方案

- 将 `LayerFolderPath` 内容直接递归复制到 workspace：拒绝；layer folder 是 HCS 存储实现，不等同于合并后的 guest filesystem，而且可能包含 VHD 与传输元数据。
- 调用旧式 `HcsExportLegacyWritableLayer`：当前不采用；现有 scratch 由 ComputeStorage VHD 格式化 API 建立，选择匹配的 `HcsExportLayer` writable-layer option。若真实目标平台证明格式不匹配，应保持 backend unavailable 并重新评估 API。
- 在线程里调用同步 HCS export 并忽略 deadline：拒绝；调用不可中断，超时后仍可能持有 layer 和 staging 的资源 owner。

## 影响与迁移

新增平台 crate 内部 adapter，不改变数据库、用户配置、IPC、权限策略或现有执行行为，无需重置。adapter 尚未被生产路径调用。后续 worker wiring 必须拥有 export process/thread、attempt lease 与 bounded staging 生命周期；未知 HCS 结果格式或 cleanup 失败都应阻止 output review 和 workspace promotion。

## 验证

- `cargo fmt --package haven-platform -- --check`、`cargo check --locked -p haven-platform`、`cargo test --locked -p haven-platform`、`cargo clippy --locked -p haven-platform -- -D warnings` 通过；测试结果 124 passed、1 ignored。
- mock 链路验证固定 parent layer JSON、`IsWritableLayer` 选项、非空目录拒绝、预取消不调用 exporter、导出后取消以及只提取 workspace 子树。
- 未调用真实 HCS/ComputeStorage API，未启动 guest，也未执行 quota acceptance。
- 参考：[Microsoft HcsExportLayer](https://learn.microsoft.com/en-us/virtualization/api/hcs/reference/hcsexportlayer)、[HCS ExportLayerOptions schema](https://learn.microsoft.com/en-us/virtualization/api/hcs/schemareference)、[Microsoft HcsExportLegacyWritableLayer](https://learn.microsoft.com/virtualization/api/hcs/Reference/HcsExportLegacyWritableLayer)、[hcsshim ComputeStorage ExportLayer](https://github.com/microsoft/hcsshim/blob/main/computestorage/export.go)、[hcsshim ComputeStorage Layer schema](https://github.com/microsoft/hcsshim/blob/main/internal/hcs/schema2/layer.go)。

## 回滚

可移除 adapter、module 注册和本 ADR；它未接入 provider，不影响现有执行行为。保持 broker quota gate closed，直到隔离 worker、真实 transport 格式和 guest quota 证明完成。
