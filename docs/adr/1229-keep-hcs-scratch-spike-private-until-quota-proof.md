# ADR 1229：配额验收前保持 HCS scratch 原语为 crate 内部

## 状态

已实施。HCS VHDX 创建与格式化 spike 只在 `haven-platform` crate 内可见；quota runtime gate 未通过前，不向 Tools、App 或其它 crate 提供调用 API。

## 背景

ADR 1227/1228 增加了 `BoundedScratchVhd`，但它只证明 VHDX 的虚拟最大容量，HCS formatter 在当前 token 下还返回 `ERROR_PRIVILEGE_NOT_HELD`。原型此前通过公开 `windows_container::scratch_vhd` 和公开构造/清理方法避免 dead-code 警告；这会提前形成一个没有生产语义保证的平台 API，让普通 App/Tools 调用路径绕过将来的 broker lifecycle owner。

ADR 1226 明确要求在运行期超额写入证明之前，不接入 broker IPC、installer 或工具执行。因此原型应该可在 `haven-platform` 的隔离 probe 中研究和验证，但不应成为跨 crate 能力。

## 决定

- `scratch_vhd` 改为 `pub(crate)`，scratch owner 及其创建、格式化、layer path 和 cleanup 操作只允许平台 crate 内使用。
- 非测试构建仅对该尚未接入的 prototype 声明有理由的 `dead_code` expectation；单测构建不抑制 lint，确保测试继续消费这条代码。未来 provider 只有在 ADR 1226/1220 runtime gate 通过、broker lifecycle 设计完成后，才能在同 crate 内消费并移除 expectation；不得为消除 warning 再公开实现模块或构造器。
- 不改已稳定的 `WindowsContainerExecutionPort`，不把 scratch owner、host layer path、HCS handle 或 privileged operation 暴露给 Tools/App；当前没有生产 consumer。
- HCS quota gate 未通过时，shell/skill/MCP 的 WorkspaceWrite 行为保持 fail closed。此封装调整本身不构成 HCS quota 或格式化成功的证据。

## 替代方案

- 维持公开 scratch VHD API 供未来调用方直接使用：拒绝；会把未完成的存储 owner 生命周期和特权格式化变成跨 crate 合约。
- 删除 scratch spike：暂不选择；VHDX 最大容量创建和实际 Windows 错误码为后续 quota 验收提供具体实验基线。
- 用 `allow(dead_code)` 隐去未接入原型：拒绝；带理由的 `expect` 会在原语被接入后提醒移除临时抑制。

## 影响与迁移

只缩小未发布的 Rust 内部 API；无跨 crate 调用方、数据库、配置、IPC、授权或用户数据变化，无需重置。临时 `dead_code` expectation 在 provider 正式使用原语时删除。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --locked -p haven-platform`
- `cargo clippy --locked -p haven-platform -- -D warnings`
- `scripts/check-adr-index.ps1`
- 依旧没有 HCS privileged format、compute-system 或 guest 超额写入验收；现有 ignored probe 不作为通过证据。

## 回滚

若通过 runtime Gate 并完成平台内 provider，可在 `haven-platform` 内部接入该能力并移除 `dead_code` expectation；不要恢复跨 crate 公开 scratch 实现 API。若 runtime Gate 无法通过，则删除 spike 并保持 fail-closed 策略。
