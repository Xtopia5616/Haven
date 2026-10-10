# ADR 0872：统一模型目录发现命令

## 状态

Accepted — 2026-10-10

## 背景

设置页存在两条模型目录刷新路径。`discover_models` 对单个配置连接返回 `Vec<ModelInfo>`，支持当前 UI 草稿传入 endpoint、认证方案与代理；当请求没有显式 API key 时，App 只会在连接名对应的已配置 Base URL 匹配时解析存储凭据。`discover_all_models` 则从 App 已保存配置另行遍历 Provider、拼装 auth header、代理并在后台并行请求。

两条路径重复维护 provider 请求语义，且批量路径读取持久配置而非设置页当前草稿。它还把请求失败映射为空数组，导致“有效空目录”和“请求失败”无法区分；UI 的单连接 helper 又用 `models.length > 0` 判断成功，把有效空目录报告为失败。

## 决定

- 删除 `discover_all_models` Tauri handler、注册项、contract metadata、UI invoke helper 与 generated command entry；不提供兼容命令或别名。
- 设置页所有模型目录刷新统一通过 `modelDiscoveryCommands.ts` 调用 `discover_models`。全量刷新在 UI 按当前 Provider 草稿并行 fan-out，并对每个连接单独聚合 resolve/reject，保留其它连接的成功结果。
- 连接请求使用当前设置草稿中的 `provider_name`、Base URL、proxy/no-proxy 与显式 API key。只有 auth header 与 `Authorization: Bearer` 默认不同才发送 override；默认方案由 App 根据已配置供应商协议推导，避免 Anthropic/Gemini 的默认认证被误写为 Bearer。存储凭据仍由 App 按已配置连接名和匹配 Base URL 解析；renderer 改过 endpoint 但未提供凭据时，不能把存储密钥发送到不同地址。keyless provider 继续显式请求无认证目录。
- UI helper 以 promise resolve 作为成功，包括空数组；reject 作为失败。UI 只把成功目录写入缓存；同一连接配置目标下的失败保留最后一次成功缓存。保存后的 endpoint、身份、凭据引用/值、auth scheme 或代理变化会先失效旧目录；skip、删除或请求中途配置变化也会清除旧目录。响应提交前比较完整 discovery target；过期响应不进入缓存，仍有效的新目标最多重试一次。
- 本地刷新过程状态只属于 ModelSettings/controller，不生成第二套 IPC outcome DTO。`provider_name` 表示配置连接名；`ModelInfo.provider` 继续表示供应商身份，二者不混用。

## 替代方案

- 保留 `discover_all_models`，仅为它新增 outcome DTO：拒绝。它仍会与单连接 handler 重复保存凭据解析、auth header、代理和静态目录策略。
- 让单 Provider 失败拒绝整个 bulk 操作：拒绝。UI 并行独立调用后可保留部分成功，并按连接汇总错误。
- 用空列表或列表长度推断成功：拒绝。空目录是合法成功响应；网络/协议失败通过 invoke rejection 表达。
- 用持久化配置结果刷新未保存的 UI 草稿：拒绝。设置页的临时 endpoint/auth/proxy 必须与发出的请求对应；存储密钥仍须通过 App 的 endpoint 匹配保护。

## 影响与验证

- Tauri command 数减少一项；当前 IPC contract checker 验证 79 个 handler。Generated command map、命令安全目录、IPC 文档和 command owner 检查同步更新；Rust/TypeScript IPC 输出不再包含 provider-keyed discovery result map。
- Rust 配置、数据库 schema、持久化数据与模型路由均不变；无兼容 alias、迁移或 reset。
- `cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、IPC contract generation/check（79 handlers）、`corepack pnpm run check` 与 `corepack pnpm run build` 均通过；测试套件未运行。

## 回滚

如需回滚，需恢复 batch handler、Tauri 注册、command metadata、安全目录、generated contract 与 UI 调用分支，并同时恢复其配置读取、凭据、认证、代理和失败状态语义。仅恢复旧空数组结果会重新引入状态混淆；无需数据库、配置或用户数据回滚。
