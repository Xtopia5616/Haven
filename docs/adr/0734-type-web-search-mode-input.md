# ADR 0734：联网搜索设置复用生成模式 enum

## 状态

已采纳并实施。

## 背景

`haven_llm::WebSearchMode` 已拥有闭合的 `Off`、`Auto`、`Always` runtime 词汇，`set_web_search` handler 也只接受这三个规范值，但 IPC 签名仍是 `Option<String>`。handler 必须 trim/lowercase 并验证，generated request 与 UI toolbar callback 因而保持开放字符串。

唯一生产 UI caller 是 `+page.svelte` 的 `ModelToolbar`，由 `chatModelOperations.selectWebSearch` 调用；choice 只提供 off/auto/always。切换模型时也只会将不支持的搜索关闭，或将 Gemini 的 always 降为 auto。请求带 `request_kind`，后端只修改该路由当前分配的模型；auto/always 会在保存前检查 provider wire capability，off 不需要该 capability。

`mode: null` 不是 `off`：它删除模型的显式 `web_search` 覆盖，让 runtime 按 `HAVEN_WEB_SEARCH` 环境变量解析，再默认 off。配置仍以 `Option<String>` 存储；runtime parser 还为 provider/environment 输入兼容 `required`、`on`、`1`、`true` 等 alias。模型配置成功后运行时 LlmRouter 会 hot-swap。常规 toolbar 在命令成功后更新本地状态；模型切换时的兼容性规范化是后台调用，失败会报告错误。

## 决定

- `WebSearchMode` 派生 Serde，并以 snake_case 序列化，使 `set_web_search` 的参数收窄为 `Option<WebSearchMode>`，IPC generator 导出 `WebSearchModeInput`。
- `ModelToolbar` 的 web search option/callback 与 `chatModelOperations.selectWebSearch` 引用 generated union；`ModelToolbar` 的 model snapshot/current display 保留 string 边界，因为它读取的是开放配置 projection。
- handler 从 enum 映射到已有规范配置文本；保留 null 清除覆盖、RequestKind 路由、provider capability gate、配置写入与 runtime hot-swap 逻辑。
- runtime 的宽松 `parse_web_search_mode` 继续处理存量配置及 environment aliases；Tauri setter 只接受三个规范小写 JSON 值。

## 替代方案

- 保留 `Option<String>` 并让 handler 解析：拒绝。命令和唯一 UI caller 的值域已经闭合，重复字符串解析没有额外兼容价值。
- 把 `null` 映射成 `off`：拒绝。此举会屏蔽 `HAVEN_WEB_SEARCH` 覆盖，改变未配置模型的运行时语义。
- 将配置字段改成 enum：暂缓。配置/environment parser 有独立的兼容别名与未配置回退语义；本 ADR 只收窄用户 setter 的 IPC 值域。

## 影响与验证

生成的 request `mode` 从任意 string 改为 `WebSearchModeInput | null`。UI 控件只能提交三个 canonical modes；null 仍可用于显式清除 model override。无数据库或 schema 变化。无效/非规范 IPC 值在 Serde 解码阶段拒绝，provider capability 与 config apply 失败仍由既有命令错误返回。

验证：`cargo fmt --all -- --check`、`cargo check --workspace --locked`、`cargo clippy --workspace --locked -- -D warnings`、`cargo test --workspace --locked`、UI `check` / `test:run`（125 files / 992 tests）/ `build`、`scripts/check-ipc-contracts.ps1`（80 handlers）、`scripts/check-ipc-events.ps1`（35 channels）、`scripts/check-adr-index.ps1`（717 ADRs）与 `git diff --check` 均通过。

## 回滚

如回滚，需恢复 `WebSearchMode` 的 IPC Serde 能力、handler `Option<String>` parser、generated request、toolbar/controller 类型、IPC/naming/architecture 文档及本 ADR 索引；不涉及持久数据迁移。
