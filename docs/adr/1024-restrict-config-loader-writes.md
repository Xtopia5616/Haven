# 1024：将 ConfigLoader 的生产写入收归 ConfigService

## 状态

已采纳并实施（2026-10-12）。

## 背景

ADR 0068 已将 `ConfigService` 定为进程内配置编辑、持久化与 runtime apply 的唯一 owner。但 Common 的 `ConfigLoader` 仍把 `save`、`config_mut` 和 `apply_settings` 作为跨 crate 公共入口；下游可绕过 Service 的编辑 gate、版本通知与 apply 流程。生产调用图没有此类消费者，唯一 App 下游写入来自设置命令测试夹具。

## 决定

- `ConfigLoader::save` 与 `ConfigLoader::config_mut` 收窄为 `pub(crate)`，仅 Common 配置服务及模块内测试可使用。
- `ConfigLoader::apply_settings` 只在 Common 单元测试构建中编译，并设为私有；生产 Settings 更新继续由 `ConfigService` 执行。
- App 设置命令测试夹具改用 `ConfigService::edit` 初始化配置，并以 Service 初始版本计算后续版本断言。
- 为三个受限写入口增加 `compile_fail` 文档测试，保证下游 crate 不会重新取得绕过 Service 的写权限。

## 影响

配置文件格式、Settings IPC、持久化行为及 runtime apply 顺序均不变。生产依赖仍可读取 `ConfigLoader::config()` 并在 composition 阶段构造 `ConfigService`，但不能直接修改或保存 loader 状态。

## 替代方案

- 保留公开写入口并依赖调用方自律：拒绝，调用图已确认没有跨 crate 生产用途，公开可见性会再次形成旁路。
- 删除 `ConfigLoader` 的读取 API：拒绝，App composition 与 Tools 当前读取配置仍需要加载结果。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`，包含 Common 的三个可见性 `compile_fail` 文档测试
- `cargo check --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- `corepack pnpm run check`、`corepack pnpm run test:run`、`corepack pnpm run build`
- IPC contract 与 UI owner checker
- `git diff --check`

## 回滚

若未来出现真实跨 crate 配置初始化需求，应为该需求增加 `ConfigService` 构造/编辑能力；不重新公开底层 loader 的可变状态或写入方法。
