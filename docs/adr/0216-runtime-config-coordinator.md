# ADR 0216：RuntimeConfigCoordinator 日志级别应用切片

- 状态：已采纳（阶段 5 最小纵向切片）
- 日期：2026-09-23
- 范围：`haven-app-binary` 的运行时日志级别应用
- 关联：[架构降复杂度重构路线图阶段 5](../architecture-refactor-roadmap.md#阶段-5已完成runtime-config-apply-ownershipp1)、[ADR 0068](0068-versioned-config-service.md)

## 背景

路线图阶段 5 的目标是逐步把配置提交后的运行时应用收口到协调边界。当前日志配置由
`ConfigService` 保存，设置命令和受限 admin surface 分别直接修改 tracing reload handle；
两处重复构造同一 `EnvFilter`，但有意采用不同失败策略。

## 决定

1. `ConfigService` 继续作为持久配置的唯一权威。`RuntimeConfigApplyPlan` 仍负责识别日志
   domain；新增的 `apply_log_level_to_handles` 只复用运行时过滤器应用，不读取、修改或
   持久化配置。
2. 共享应用函数接收 reload handles 与 `LogLevel`，为每个 handle 设置
   `EnvFilter::new(format!("haven={}", level.as_str()))`。reload 错误以
   `anyhow::Result` 返回，不在协调函数内吞掉。
3. `settings::update_settings` 将全部 handles 一次交给该函数，并在首个 handle 失败时
   返回现有的命令错误。配置 patch 仍先由 `ConfigService` 持久化；如果之后应用失败，
   本切片不增加运行时回滚。
4. `ReloadLogLevelPort` 为每个 handle 单独调用同一函数。失败时逐项记录经
   `sanitize_error_text` 处理的警告，继续处理其余 handles，最后仍返回 `Ok(())`，保持
   admin tool 的 best-effort 语义。
5. 本 ADR 仅完成阶段 5 的日志级别切片，不引入覆盖其他配置域的通用 coordinator，也不
   改变日志配置、wire、IPC 或运行时所有权。

## 替代方案

- 在 settings 与 admin port 中继续各自构造 `EnvFilter`：会保留两份运行时应用逻辑。
- 一次性将全部配置域迁入 coordinator：超出本切片的文件写集和行为范围，后续领域应按
  独立切片迁移并分别验证。
- 让 admin port 将 reload 失败传播为硬错误：会改变管理工具既有 best-effort 契约。

## 影响与验证

- TOML 配置、Tauri 命令/事件、IPC 和 tracing filter 格式保持不变；无需数据或配置重置。
- 单元测试通过两个独立 reload layer 验证所有 handles 都收到相同 level，不初始化全局
  tracing subscriber。
- 验证命令：

  ```text
  cargo fmt -p haven-app-binary
  cargo check --locked -p haven-app-binary
  cargo test --locked -p haven-app-binary --lib
  cargo clippy --locked -p haven-app-binary -- -D warnings
  git diff --check
  ```

## 回滚

删除共享应用函数并恢复两个调用点的旧实现，同时移除本 ADR 与索引项即可。无需重置配置、
数据库或运行时数据。
