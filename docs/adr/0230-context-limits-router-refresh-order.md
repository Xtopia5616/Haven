# ADR 0230：ContextLimits 路由刷新与应用顺序

- 状态：已采纳（2026-09-24）
- 范围：`haven-app-binary` 的 ContextLimits 运行时计划与应用顺序
- 关联：[ADR 0068](0068-versioned-config-service.md)、[ADR 0216](0216-runtime-config-coordinator.md)

## 背景

`ConfigService` 将 context limits 单独归类为 `ConfigDomain::ContextLimits`。
设置命令会用持久化后的同一配置快照构建 `LlmRouter`，其中缓存
`default_context_window`、`max_response_tokens` 和 `reasoning_echo_max_chars`。
若该 domain 只更新 Tools 与 Agent，router 会继续使用旧值。

`hot_swap_router` 还会从配置构建 STT、OCR、TTS 和图像生成客户端。当前实现先构建全部
依赖，再替换 Agent 与 Tools 的 router/media 引用；构建失败会返回现有命令错误。
配置已在进入运行时应用前持久化。

## 决定

1. `RuntimeConfigApplyPlan` 将 `ContextLimits` 同时映射为 live targets
   `ContextLimits` 与 `LlmRouter`。重复 domain 仍经 `push_live` 去重；该变更不增加
   `restart_required` 项，也不改变其他 domain 的映射。
2. 设置命令在 `hot_swap_router` 成功后，才调用 Tools 与 Agent 的
   `set_context_limits`。Limits-only 变更因此刷新 router 缓存的三个限制值以及已有的
   Tools/Agent context-limit 消费者。
3. 若 router 或任一依赖客户端构建失败，`hot_swap_router` 按原路径返回错误；所有 router
   依赖在构建成功前不会被替换，Tools/Agent 的 context limits 也尚未更新。持久化配置不
   回滚，调用方仍收到既有错误。
4. 设置命令不是全域事务：其他 domain 仍按既有顺序应用；本决定不增加运行时补偿回滚，
   也不改变 router 成功后的其他应用失败语义。
5. 不引入通用订阅协调器，不改 schema、IPC、provider 或配置格式。

## 替代方案

- 仅更新 Tools/Agent：会令 router 保留旧的 context window、response token 和 reasoning
  echo 限制。
- 在 router 刷新前更新 context limits：router 构建失败时会让 limits 消费者与旧 router
  跨代运行。
- 为所有设置应用引入原子协调器或运行时回滚：超出本切片范围，且会改变既有失败语义。

## 影响与验证

- ConfigService 持久化顺序、配置快照、TOML、schema、IPC 和 provider 行为不变；无需重置。
- 纯计划单测覆盖单独 `ContextLimits` 的两个 live targets、router target 去重，以及既有
  restart/live 边界。`hot_swap_router` 的依赖构建失败由现有错误传播路径处理；此处通过
  应用顺序保证 limits 更新发生在其成功返回之后。
- 验证命令：

  ```text
  cargo fmt --all -- --check
  cargo test --locked -p haven-app-binary
  cargo check --workspace --locked
  cargo clippy --locked -p haven-app-binary -- -D warnings
  git diff --check
  ```

## 回滚

恢复 ContextLimits 的单 domain 映射和设置命令原有应用顺序，并删除本 ADR 与索引项。无需
重置配置、数据库或运行时数据。
