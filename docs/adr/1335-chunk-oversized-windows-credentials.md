# ADR 1335：在 Windows Credential Manager 中分块存储超限凭据

- 状态：已接受
- 日期：2026-10-20
- 相关：ADR 0401、ADR 1319、`docs/development-standards.md`

## 背景

ChatGPT OAuth 回调已成功到达 Haven，但部分账户凭据序列化后超过 Windows Credential Manager 单条 generic credential 的 2560 字节上限，导致 `complete_chatgpt_plan_login` 保存失败。OAuth 回调页在 token 交换与本地安全存储前就已返回 HTTP 成功，因此页面显示收到授权，而 Haven 最终报告登录失败。

## 决定

- 继续只使用 Windows Credential Manager 保存凭据，不将令牌写入配置文件、普通文件或日志。
- 不超过单条上限的现有值继续按原格式存储；读取仍兼容既有单条凭据。
- 超限值最多存储为 32 个 UTF-8 安全分片，每片不超过 2048 字节，整体最多 65,440 字节。根 target 保存版本化 manifest、活动分片槽位、分片数与 SHA-256 完整性摘要。
- 更新超限凭据时先写入非活动槽，再写 manifest 提交；提交成功后清理旧槽。读取遇到 manifest、缺失分片、无效 UTF-8 或摘要不匹配时失败关闭。
- 删除凭据时清理根 target 与两个分片槽。清理失败只记录不含凭据内容的固定 warning。
- OAuth 回调页使用内嵌静态样式，无脚本或外部资源；成功页明确表示 Haven 仍需完成验证与安全保存。

## 替代方案

- 截断或拒绝大凭据：拒绝，无法可靠保存 provider token。
- 将 token 放进普通文件或配置：拒绝，会扩大明文凭据的暴露面。
- 更换为文件加密方案：拒绝，当前 Windows Credential Manager 已满足静态秘密存储要求，分片仍受同一 OS 安全边界保护。

## 影响与恢复

不影响 config 或数据库 schema，不需要重置用户数据。旧版单条凭据可由新版本读取。旧版程序无法读取新版本写出的分片 manifest；若回滚代码后 ChatGPT 登录凭据不可用，用户需重新登录。删除操作发生部分系统错误时可能留下不可读取但仍由 Credential Manager 加密的孤立分片，程序会返回清理错误并记录脱敏 warning。

## 验证

- 私有存储后端替身覆盖旧格式读取、大凭据 Unicode 往返、槽位切换、manifest 提交失败保留旧值、缺失/损坏分片拒绝、删除清理与容量上限。
- Windows 环境执行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-platform credentials::tests`、`cargo test --locked -p haven-app-binary chatgpt_plan_login::tests`、`cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings`。测试替身不触碰用户的真实凭据库；实际 ChatGPT OAuth 需在 Haven 中手动重试确认。
