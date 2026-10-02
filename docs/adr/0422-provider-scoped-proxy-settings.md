# ADR 0422：Provider 级代理设置

## 背景

LLM 请求默认跟随 Haven 进程的环境代理，无法按 Provider 绕过失效的系统代理或选择单独的代理端点。模型目录发现另建 HTTP client，之前也没有读取 Provider 的代理配置，导致设置页发现结果和实际请求的网络路由可能不同。

## 决定

- 在 Provider 编辑界面提供三种路由：默认环境代理、直连、指定 HTTP(S) 代理。指定代理可配置逗号分隔的绕过主机列表。
- 复用已有 `proxy_url` / `no_proxy` 配置字段，不新增配置格式或持久化迁移：`proxy_url = None` 表示默认环境代理，空字符串表示直连，非空字符串表示指定代理；`no_proxy` 只对指定代理生效。
- 同一 Provider 的模型请求、单个 Provider 模型发现、批量模型发现和媒体模型发现使用相同代理配置。直接调用且没有 Provider 配置的旧 `ModelRegistry::discover_from` 保持默认环境代理行为。
- 代理地址只接受 HTTP(S)，不接受内嵌账号密码，避免将代理凭据写入普通配置文件。配置调试输出隐藏代理地址。

## 替代方案

- 只支持进程环境变量：无法为特定 Provider 直连或指定独立代理，拒绝。
- 另增全局代理设置：会影响所有 Provider，无法解决按服务分流的需求，拒绝。
- 将代理账号密码直接保存在代理 URL：配置文件不具备代理凭据的安全存储边界，拒绝；需要认证代理时应另行设计 Credential Manager 引用。

## 影响与回滚

旧配置中没有代理值时继续使用环境代理。此变更不改变 schema 版本，也不要求重置配置。回滚代码会忽略新增 UI 写入的既有可选字段；需要保留路由选择时应先记录 `proxy_url` 与 `no_proxy`，而后再回滚。

## 验证

- 前端覆盖默认、直连、自定义代理、绕过主机和模型发现参数；Rust 覆盖直连、代理绕过匹配与认证凭据拒绝。
- 通过：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、`corepack pnpm run check`、`corepack pnpm run test:run`（900 项）、`corepack pnpm run build`、`pwsh -NoProfile -File scripts/check-ipc-contracts.ps1`（75 个命令契约）。
