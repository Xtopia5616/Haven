# ADR 0862：跨 crate 共用 canonical JSON 编码

## 状态

Accepted — 2026-10-10

## 背景

Tools 的授权确认输入 hash 与 LLM 的 schema/cache identity 各自递归排序 JSON 对象键，并保留数组顺序；两份实现语义相同，但分别位于 `haven-tools::security` 与 `haven-llm::types`。两 crate 已依赖 `haven-common`，而架构规定 Common 拥有被多个 crate 共享的纯数据函数。保留两份实现会使同一 JSON 值在确认与缓存身份中的确定性编码可能逐渐分叉。

## 决定

- `haven_common::json` 唯一拥有 `canonicalize_json(Value)` 与 `canonical_json_bytes(&Value)`。递归排序 object keys，保持数组顺序；bytes 函数继续通过 `serde_json::to_vec` 编码，并保留现有序列化错误时返回空字节的行为。
- Tools 的 `canonical_input_hash` 使用 Common 编码后仍在 Security 中计算 SHA-256 和十六进制摘要。确认 capability、risk、policy revision、过期时间与验证流程继续由授权 owner 管理。
- LLM 的 schema mapping 与各 adapter 的 cache identity 直接使用相同 Common 函数；原 `stable_json_bytes` 仅是同一行为的别名，删除并统一为 `canonical_json_bytes`。
- Provider request/cache identity 的字段选择、组合顺序、域分隔和 key 生成仍归各自 adapter；此函数不接管 provider wire serialization。
- 不改变 hash 输入、JSON key 顺序、数组顺序、SHA-256 结果、provider request 内容或错误路径；无 IPC、配置、数据库及持久化变化，不需要重置。

## 替代方案

- 保留 Tools 与 LLM 两份算法：拒绝。它们实现相同的递归排序规则，且有可复用的现成叶子依赖。
- 把确认 hash 或 LLM cache identity 整体移入 Common：拒绝。它们依赖不同的安全和 provider 领域语义，Common 只拥有纯 JSON 表示函数。

## 影响与验证

本次新增 Common 纯函数 owner，并删除 Tools 与 LLM 的重复 canonicalizer 和同义 byte helper。验证使用 Rust 格式检查、Common/Tools/LLM 编译及严格 Clippy；测试目标只编译、未执行测试套件。

## 回滚

若后续发现某领域需要不同的 JSON canonicalization 语义，应先定义不同且可说明的表示契约，再仅让该领域调用专属函数；恢复旧算法前需证明差异是有意的。当前无持久数据需要重置。
