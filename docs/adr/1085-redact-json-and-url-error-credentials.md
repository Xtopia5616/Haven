# ADR 1085：脱敏 JSON 与 URL 错误凭据

## 状态

已接受（2026-10-13）

## 背景

`sanitize_error_text` 是 Common 中跨 UI、通知、IPC 和内部错误日志共用的安全边界。审计发现它只识别裸 `key=value` / `key:value` 形式，未覆盖 JSON 中带引号的敏感字段，也未处理 `https://user:password@host` 的 URL userinfo；这两类错误文本会透传凭据。

## 决定

- 清洗器在保留 JSON/类 JSON 分隔符的同时替换敏感字段值，覆盖带引号的 key、单双引号值、空格和转义引号。
- 清洗 URL authority 中含密码的 userinfo；替换整个 userinfo，避免泄露用户名或密码。
- 日志策略继续禁止记录正文型数据或原始外部服务错误；清洗不能作为记录这类内容的许可。

## 影响

凭据值不再由以上格式的边界错误传到 UI、通知、IPC 或普通内部错误摘要中。替换文本仍经统一长度上限与单行归一处理。不涉及持久化、配置或 IPC shape 变更。

## 验证

- `cargo test --locked -p haven-common error::tests`
- 完整 Rust workspace 和 UI 门禁按此次跨层切片执行。

## 回滚

不得恢复不识别 JSON 敏感键或 URL 密码的实现；若清洗策略变化，应保留覆盖这些输入的回归测试。
