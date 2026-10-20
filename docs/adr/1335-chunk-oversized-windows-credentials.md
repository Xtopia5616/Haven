# ADR 1335：在 Windows Credential Manager 中分块存储超限凭据

- 状态：已接受
- 日期：2026-10-20
- 相关：ADR 0401、ADR 1319、`docs/development-standards.md`

## 背景

ChatGPT OAuth 回调已成功到达 Haven，但部分账户凭据序列化后超过 Windows Credential Manager 单条 generic credential 的 2560 字节上限，导致 `complete_chatgpt_plan_login` 保存失败。OAuth 回调页在 token 交换与本地安全存储前就已返回 HTTP 成功，因此页面显示收到授权，而 Haven 最终报告登录失败。

## 决定

- 继续只使用 Windows Credential Manager 保存凭据，不将令牌写入配置文件、普通文件或日志。
- 所有新凭据统一使用版本化 manifest 与分片格式，包括空值和小于单条上限的值；不继续支持旧版原始单条格式，不执行迁移。旧格式凭据会失败关闭，按项目 reset 流程清除当前 `config.toml` 并重新录入密钥。
- 一个值最多存储为 32 个 UTF-8 安全分片，每片不超过 2048 字节，整体最多 65,440 字节。根 target 保存 manifest、活动分片槽位、分片数与 SHA-256 完整性摘要。
- 更新凭据时先写入非活动槽，再写 manifest 提交；提交成功后清理旧槽。读取遇到无效 manifest、缺失分片、无效 UTF-8 或摘要不匹配时失败关闭。
- 删除凭据时清理根 target 与两个分片槽。清理失败只记录不含凭据内容的固定 warning。
- OAuth 回调页使用内嵌静态样式，无脚本或外部资源；成功页明确表示 Haven 仍需完成验证与安全保存。

## 替代方案

- 截断或拒绝大凭据：拒绝，无法可靠保存 provider token。
- 将 token 放进普通文件或配置：拒绝，会扩大明文凭据的暴露面。
- 更换为文件加密方案：拒绝，当前 Windows Credential Manager 已满足静态秘密存储要求，分片仍受同一 OS 安全边界保护。

## 影响与恢复

不影响数据库 schema。新版本不读取旧格式的任何 provider/MCP 凭据；引用这些凭据的旧 `config.toml` 会 fail closed，用户需按 `docs/release-and-reset.md` 重置当前配置并重新录入密钥。重置配置不会删除 Windows Credential Manager 中失去引用的旧条目。旧版程序也无法读取新格式；若回滚代码，需再次重置配置并重录凭据。删除操作发生部分系统错误时可能留下不可读取但仍由 Credential Manager 加密的孤立分片，程序会返回清理错误并记录脱敏 warning。

## 验证

- 私有存储后端替身覆盖旧格式拒绝、小/空/大凭据新格式往返、Unicode 分片、槽位切换、manifest 提交失败保留旧值、缺失/损坏分片拒绝、删除清理与容量上限。
- Windows 环境执行 `cargo fmt --all -- --check`、`cargo test --locked -p haven-platform credentials::tests`、`cargo test --locked -p haven-app-binary chatgpt_plan_login::tests`、`cargo test --workspace --locked` 与 `cargo clippy --workspace --locked -- -D warnings`。测试替身不触碰用户的真实凭据库；实际 ChatGPT OAuth 需在 Haven 中手动重试确认。
