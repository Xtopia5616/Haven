# ADR 0086：错误传播边界与失败安全

## 背景

传输、流式读取、后台通知、ReAct 投影、恢复数据和设置页的若干路径曾把错误转换为空值、默认值或看似成功的状态。这样会把网络中断、数据库失败、损坏的持久化数据和 worker 失效伪装成正常完成，造成状态不可恢复或用户无法判断实际结果。

## 决定

- LLM 代理、HTTP client、认证 Header 和错误响应体构造失败必须保留为显式错误；适配器工厂使用不可用 client 保留该错误，首次操作时返回原始配置失败，而不是改用默认 client。
- 流式 framing 区分干净 EOF 与底层传输错误；错误进入下游并终止本次 stream，不得 flush 成功的半截响应。
- ReAct 的持久化投影先成功写入数据库，再发布 UI/事件；投影错误向调用方传播。仅恢复期 error partial 等已文档化的旁路允许记录错误后继续，并且不得伪造正常事件。
- 后台/定时任务只有在完成事件成功送入 channel 后才标记已发送/完成；channel 已关闭时保留可重试状态并记录错误。
- 恢复持久化任务时拒绝无效时间、模式、参数和 JSON，不以默认值修复可能立即执行的任务。
- MCP 发现失败、VAD worker 失效、shell 探测失败和 provider 模型刷新失败均保持离线/失败状态，前端成功提示只在所有目标成功时显示。

## 影响与回滚

错误现在会更早暴露，部分原先“继续运行”的路径会明确失败或等待重试；这可能改变异常场景下的 UI 提示和恢复行为，但不改变正常请求协议或数据库 schema。回滚本 ADR 及对应提交即可恢复旧行为。

## 验证

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --locked -- -D warnings`
- `cargo test --workspace --locked`
- `cd ui; corepack pnpm run check`
- `cd ui; corepack pnpm run test:run`
- `cd ui; corepack pnpm run build`
