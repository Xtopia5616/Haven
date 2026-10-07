# ADR 0644：统一 Ask 响应 view 类型

## 状态

已采纳并实施。

## 背景

一次 resolved Ask interaction 的响应字段 `answer?: string` 与 `ignored?: boolean` 被 UI 各层重复声明：chat interaction controller 对 `request.response` 做 cast、chat message projection 消费同一结构、SessionMessage/reducer action 保存 `resolved` view，ChatBubble 与 ToolResultCard 接受该 view。通用 `InteractionRequest.response` 还承担 ask、confirm 等不同交互结果，保持为 `unknown` 才能保留其动态边界。

## 决定

1. 在 App interaction contract 中声明 `AskResponseView { answer?: string; ignored?: boolean }`，名称明确它是从动态 interaction response 读取的 renderer view，不冒充 wire DTO。
2. controller、message projection、reducer state/action 与两个消息组件全部引用同一个类型。
3. 字段仍为 optional，保持当前答复与忽略消息的处理；不将通用 `InteractionRequest.response` 收窄为 Ask-only shape。

## 替代方案

- 每层继续内联同样字段：拒绝。相同 UI 视图契约会漂移。
- 把通用 `InteractionRequest.response` 改为 `AskResponseView`：拒绝。其它交互 kind 的响应不属于 Ask。
- 将其提升为 generated Rust wire DTO：拒绝。该结构只是 UI 对动态 response 的 Ask 专用解释，当前 backend envelope 保持动态。

## 影响与验证

仅统一静态 view 类型 owner；Ask lifecycle、response payload、resume 与 UI 呈现行为不变。无需保留本地旧 shape。验证：`corepack pnpm run check`、`corepack pnpm run test:run`、ADR 索引与 staged diff 检查。

## 回滚

如需回滚，恢复各层内联 Ask response shape，并同步撤回命名规范、路线图和 ADR 索引；无数据库或 IPC 字段变化。
