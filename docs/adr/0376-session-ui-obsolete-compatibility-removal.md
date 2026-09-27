# ADR 0376：删除会话 UI 的过期兼容层

- 状态：已采纳（2026-09-27）
- 范围：会话错误原因读取/写入与 resume interaction DTO 规范化
- 关联：[ADR 0245](0245-session-error-cache-in-reducer.md)、[ADR 0350](0350-session-ui-field-mapping-audit.md)

## 背景

ADR 0245 将会话错误原因并入 `SessionReducer`，但仍保留 `sessionErrorStore` 转发函数；剩余调用方已经能直接使用 reducer 的 getter 和 action。恢复交互的 Rust 投影始终使用 snake_case，前端 normalizer 还接受 camelCase 别名，并为缺少字段执行字符串/数字 coercion 和时间默认值。

## 决定

1. 删除 `sessionErrorStore` 及其测试；页面直接读取 `appSessionReducer`，生命周期 handler 通过已有的 `dispatchSession` 写入和清除错误原因。
2. resume interaction normalizer 只接收 Rust `InteractionRequestedEvent` 的 snake_case 字段；不再读取 camelCase 别名，也不再为缺失 `created_at` 生成当前时间或 coercion 非字符串字段。
3. 保留 envelope/row 校验：缺少必需字段、未知 kind/status 或字段类型错误的行仍被过滤。live app event 继续由 `mapAppEvent` 按其 camelCase UI contract 单独映射。
4. 不改变错误原因的内存生命周期、resume 投影、IPC、数据库、事件顺序或 X12 写入契约。

## 影响、重置与验证

- 没有持久化格式、配置或 IPC 变化，不需要用户数据重置。
- 更新 reducer、session lifecycle handler 和 resume normalizer 的 UI 回归测试；运行 UI 检查与测试。

## 回滚

恢复 `sessionErrorStore` 转发模块和 camelCase/默认值 normalizer 分支，并还原对应测试、路线图记录与 ADR 索引；无需数据迁移。
