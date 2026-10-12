# ADR 1069：统一媒体与联网搜索配置语义

## 状态

已接受（2026-10-13）

## 背景

配置解析、Settings 更新和运行时曾对 OCR、STT、TTS、图像生成及联网搜索的空值、别名和 provider 引用采用不同规则。这样配置可能通过编辑器保存，却在运行时被静默解释成另一个选择；Settings 的模型继承状态也无法清楚地区分“继承”和显式关闭。

## 决定

- OCR provider 只接受 `llm`、`baidu`、`azure`、`tencent`、`none`。
- STT `provider_name` 去除首尾空格；`llm`、`mcp`、`none` 作为大小写不敏感的保留值。其它名字按精确 provider 连接名解析；`mcp` 必须引用已配置的 MCP server。
- TTS 与 image generation 的 provider 名称为空或大小写不敏感的 `none` 时禁用；否则必须匹配已配置的 provider 连接名。
- Web search 归一化时去除首尾空格并忽略大小写；canonical 值为 `off`、`auto`、`always`，同时接受 `required`、`on`、`1`、`true` 作为 `always` 的别名。空值/空白继续继承 `HAVEN_WEB_SEARCH`，显式 `off` 才关闭。
- Settings 解析响应和应用 patch 使用同样的语义校验。模型联网搜索选择按 model override、provider default、环境 fallback 的顺序解析；UI 内部的 `inherit` 仅表示清除 model override，不作为持久配置值。
- 运行时 STT 对 reserved provider 值作相同归一，并对无效或空 provider 明确报错。

## 影响

未知 OCR/search 值、悬空 provider/MCP 引用会使 TOML 加载失败、备份原文件并在当前进程使用默认配置；Settings 更新拒绝无效值。配置结构与数据库 schema 未变，无需重置数据库。旧配置中大小写或别名值按上述语义保留。

## 验证

- Common 测试覆盖语义闭集、空值、别名、provider 引用和 MCP 引用。
- Loader 测试覆盖未知值的备份与默认恢复。
- UI contract/Settings 测试覆盖同一输入边界以及继承优先级；STT runtime 测试覆盖 reserved 值归一和错误路径。
