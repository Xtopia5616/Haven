# ADR 1225：在容器执行端口处限制输出

## 状态

已实施于平台 typed execution port；尚无 HCS provider 或生产工具接线。

## 背景

ADR 1223 的端口把原始 `FnMut` 输出回调交给 provider，并在结果构造时才截断保存的 stdout/stderr。未来 provider 若忽略契约，仍可能将超限 live bytes 发给 Tools/UI；若先完整缓存再构造结果，也可能超过端口声明的内存上限。

## 决定

- provider 改为接收 `WindowsContainerOutputSink`，不能直接访问 live-output callback。
- sink 按 stdout/stderr 独立累计各自请求上限；每个 chunk 只保留并发布尚未超限的前缀，超出部分丢弃并设置对应 truncated 标记。捕获对象最大为两个 stream 上限之和；超限不会阻止 provider 继续 drain OS pipe。
- `WindowsContainerExecutionResult` 只能从 sink 完成的 `WindowsContainerCapturedOutput` 构造，避免 provider 通过未经限制的 Vec 绕过最终结果上限。
- provider 若需要在 deadline、取消或 quota 失败时停止 guest，仍必须终止进程树并等待归零；sink 的输出截断不代表执行失败，也不允许因截断停止 drain。

## 替代方案

- 只在最终结果构造时截断：拒绝；无法限制已发布的实时输出，也无法限制 provider 中间缓存。
- 超限时对 callback 返回错误并立即停止读取 pipe：拒绝；可能让 guest 阻塞或绕过完整管道 drain/终止生命周期。
- 由各 provider 自行复制计数器与缓冲逻辑：拒绝；不同后端会产生不同限额和截断行为。

## 影响与迁移

更新平台 provider trait 的参数与结果构造方式并增加每流边界测试；无 consumer、IPC、配置或持久化变化，无需重置。后续 Tools 集成必须将 bounded sink 的 live callback 连接到已有 bounded output/event owners。

## 验证

- `cargo fmt --all -- --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --locked -- -D warnings`
- Windows platform test 验证 stdout/stderr 独立上限、边界前缀、truncated 标记及 callback 收到的字节均受限。
- 该切片不构成 HCS runtime quota proof；实际 HCS 超额写入仍是 provider 接线前门槛。

## 回滚

若移除 typed Windows Container port，可一并删除 sink 与 captured-output 类型。不能将其回滚为允许 provider 绕过限额的原始 callback 参数。
