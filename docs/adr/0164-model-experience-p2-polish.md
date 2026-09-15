# ADR 0164：模型体验 P2 清单收口

- 状态：accepted
- 日期：2026-09-15
- 范围：`haven-common`、`haven-tools`、`haven-agent`
- supersedes：ADR 0128 中尚未落地的 P2 体验打磨项

## 背景

P0/P1 已经建立了分层 capability catalog、operation view、结构化
observation 和受管媒体资产边界，但模型在首层索引、媒体结果追踪、shell
语法选择和能力判断上仍需依赖隐含约定。特别是 endpoint/model 配置存在并不等于
当前请求真的支持 vision、STT 或图像生成；窗口聚合结果和普通受管文件结果也不
总是把资产句柄放在截断安全的位置。

## 决定

1. 首层内置工具索引按 family 固定输出 `when to use`、`when not to use` 和
   `key operations` 三条短提示；只列有限的代表性 operation 和 root 计数，完整
   参数与 schema 继续以当前回合的 `tools[]` 和 `tool_catalog` 为权威。
2. `MediaResult` 的 asset 分支以 `asset_id`、导航 `notes`、operation 的顺序
   序列化。notes 要求优先读取上一条 tool result 的 `asset_id`，后续表示通过
   `media.*` view 获取；受管文件和窗口 observe 聚合结果也遵循同一顺序，并移除
   host path。
3. raw media 请求投影保留短的 `media_plan: asset_id -> representation` 标记，
   明确该 representation 已随请求发送；该标记不进入快照持久化。
4. shell 在 spawn 前保留 PowerShell 5.1 的 `&&`/`||` 硬规则，并对明显的 bash/cmd
   语法错配做保守预校验。只拦截高置信度构造，带引号的普通文本不作为语法错。
5. runtime snapshot 只报告实时 capability 状态：provider/MCP web search、vision、
   image generation、STT、录音和 TTS。它不再把模型名称、endpoint slot 配置或
   router 存在性直接呈现为模型能力；录音状态与 builtin registration 使用同一份
   configured-pipeline 判断。

## 替代方案

- 将完整 operation 清单平铺进 system prompt：信息更全，但违反分层目录和上下文
  预算目标，因此保留按需 `tool_catalog`。
- 只调整 JSON 字段顺序而不提供 notes：无法帮助模型识别下一步的资产入口，也无法
  覆盖普通 `files` 和窗口聚合结果。
- 通过执行 shell 后解析错误来反馈语法：会产生不必要的子进程和噪声，且无法满足
  spawn 前失败的安全/体验要求。
- 根据已配置的 endpoint/model 推断能力：会把不可用的 provider route 广告给模型，
  与 capability profile 和 dedicated client 的真实判断漂移。

## 影响

- system prompt 的首层工具索引增加明确标签，但仍受原有 family budget 限制。
- asset-producing tool result 增加可选 `notes` 字段并调整 JSON 序列化顺序；设备型
  media result 不虚构 `asset_id`。
- `RuntimeCapabilities` 增加 image-generation 状态，现有 web-search unavailable
  原因、媒体能力剪枝和授权边界保持不变。
- 没有数据库迁移；snapshot、provider wire content 和 host path 安全边界不变。

## 验证

- `cargo test --locked -p haven-common --lib`
- `cargo test --locked -p haven-tools --lib`
- `cargo test --locked -p haven-agent --lib`
- `cargo fmt --all -- --check`
- 重点回归：三段式工具索引预算与注入清洗、asset-first observation/notes、media
  plan 标记、shell 引号与语法错配、runtime snapshot 不泄露 `model_capabilities`。

## 回滚

回滚本 ADR 对应的代码、prompt 和测试即可；不需要数据库重置或 IPC 迁移。若只回滚
单一媒体字段，必须同时回滚 `MediaResult` 顺序、structured-first priority、文件/
窗口聚合投影和对应 notes 测试，避免同一资产在不同 producer 结果中出现不一致入口。
