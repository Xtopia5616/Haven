# ADR 0525：共享 Router 与媒体客户端构造

## 状态

已完成（2026-10-06）。

## 背景

App 冷启动与配置热更新分别从同一份 `AppConfig` 构造 `LlmRouter`、STT、TTS、OCR 和 ImageGen clients。参数映射重复，新增配置或媒体 client 时容易只改一条路径；两条路径的错误策略却有意不同：启动按单项可选能力降级，热更新在 prepare 失败时保留旧 runtime。

## 决定

1. `haven-app-binary::router_media_builder` 统一从 `AppConfig` 构造 Router、media config 和四项媒体 client。每项 client 以独立 `Result<Option<Arc<_>>, anyhow::Error>` 返回；builder 不记录日志、不决定降级，也不发布 runtime。
2. 冷启动逐项消费结果：某 client 构造失败时记录现有告警并只禁用该能力。
3. 配置热更新逐项检查结果；任一错误都会使 prepare 失败，caller 不发布新 generation。Agent 与 Tools 的发布顺序继续由 `RuntimeConfigCoordinator` 持有。
4. 不统一两种调用方的错误策略或发布生命周期。工具目录的安装、deferred 与 session scope 也不归入该 builder。

## 影响

- Router/media 构造参数只有一个实现来源；添加媒体 client 时两个入口共同获得该 client 的构造结果。
- 配置、数据库、IPC、provider schema、工具目录权限和持久数据均不变，无需重置。
- 冷启动现在更早构造媒体 clients；构造结果仍在原启动 wiring 阶段由启动 caller 处理。

## 验证

- Builder 单元测试验证单项错误与其它能力结果彼此独立。
- `cargo fmt --all -- --check`
- `cargo check --locked -p haven-app-binary`
- `cargo clippy --locked -p haven-app-binary -- -D warnings`
- `cargo test --locked -p haven-app-binary`

## 回滚

删除 `router_media_builder` 并恢复 `app_state.rs` 与 `config_runtime.rs` 中的局部构造即可。没有持久化、配置 schema 或 IPC 迁移。
