# ADR 0858：集中 filesystem reparse metadata 检查

## 状态

Accepted — 2026-10-10

## 背景

App managed-media 生命周期与 Tools `ManagedAssetRegistry` 各自实现相同的 metadata 检查：Windows 上拒绝 symlink 或任意 `FILE_ATTRIBUTE_REPARSE_POINT`，其他平台拒绝 symlink。Tools `security.rs` 另有一份相邻实现，Windows 检查 reparse attribute，其他平台检查 symlink。三处代码保护不同路径流程，但底层 OS metadata 判定相同；若谓词漂移，根目录清理、managed asset 解析和安全沙箱会出现不一致的链接拒绝规则。

路径遍历、canonicalization、非存在路径后缀、managed-root 范围和操作失败语义则因调用者而异，不能一并下沉为通用路径授权器。

## 决定

- 在 `haven-platform::filesystem::is_link_or_reparse_point` 统一单个 `std::fs::Metadata` 的 OS 相关链接判定：Windows 拒绝 symlink 或任何 reparse attribute，其他平台拒绝 symlink。
- App managed-media、Tools `ManagedAssetRegistry` 和 Tools security path resolver 共用该谓词。
- 调用者继续拥有 metadata 获取时机、路径分量遍历、canonicalization、允许根目录、UNC/device-path 规则、TOCTOU 窗口及 fail-closed 错误处理。此 helper 不替代安全网关或完整路径策略。
- 对 Tools security 原先仅查 Windows reparse attribute 的条件，明确补上 symlink type 检查；Windows symlink 本身属于 reparse point，保留同一拒绝意图。

## 替代方案

- 留下三份平台条件函数：拒绝。它们表达同一 OS metadata 谓词，复制会令 symlink/reparse 处理漂移。
- 将整段路径遍历、安全策略和目录清理移入 Platform：拒绝。ManagedAssetRegistry、上传清理与授权沙箱具有不同的根目录、恢复、取消和失败契约。
- 将 predicate 放入 Common：拒绝。它需要解释操作系统 `Metadata` 的 symlink/reparse 语义，属于 Platform 的 OS 适配职责。

## 影响与验证

这是 `haven-platform` 公共、无状态 metadata helper，以及 App/Tools 调用点调整；不改持久数据、IPC、配置或 path authorization policy，无需数据重置。现有 App staging/upload cleanup、asset registry 和 security sandbox 的链接负例测试保持原有断言；本轮按执行约束未运行测试套件。

验证通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、
`cargo clippy --workspace --locked -- -D warnings`、crate dependency guard、ADR index、
ADR Prettier 与 `git diff --check`。未运行测试套件。

## 回滚

若某个调用域需要不同的底层 metadata 策略，应以命名清晰的独立谓词表达并记录其安全理由；否则整体撤回 Platform helper 与调用点并恢复原平台检查。不加旧函数兼容 wrapper。无持久数据需要重置。
