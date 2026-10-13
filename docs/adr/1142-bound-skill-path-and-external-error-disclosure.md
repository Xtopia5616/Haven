# 1142：限制 Skill 路径与外部错误内容的传播

## 状态

已采纳并实施。

## 背景

Skills UI command snapshot 暴露绝对 `root` 并显示本机路径，而用户只需要技能状态与打开技能目录的操作。多个文件、媒体、TTS、图片生成和 MCP 错误路径还把 OS 路径或已净化的 provider/MCP 错误正文转成面向模型的 `ToolResult.error`；净化凭据不代表允许转发外部响应内容。

## 决定

- Skills `SkillInfo` wire DTO 与 `SkillInfoView` 不包含本机根路径；`SkillCard` 不显示根路径，`open_skills_dir` 仍由 Skills owner 解析并打开目录。
- Agent `skills_list` 的路径字段继续作为 ADR 0878 记录的显式工具投影例外，由 registry root owner 构造，不复用 UI command DTO。
- 文件操作和安全策略拒绝错误使用稳定类别文案，不回显本机绝对路径或 OS error body。
- STT、OCR、vision、TTS、图片生成与 MCP 远端失败的 ToolResult error 只表达稳定失败类别；provider/MCP response body 不透传进错误字段。成功的模型输出仍按既有不可信内容标记与预算处理。
- `OcrConfig::Debug` 对 base URL 使用配置状态占位文本，不输出 host/path/query。
- `media.play.file_path` 仍是受 `allowed_paths` 校验的显式本机播放入口；普通媒体派生能力不接受宿主路径或 base64。

## 影响

Skills UI 列表的 IPC shape 删除 `root`，并已重新生成核验 IPC types；本机路径和外部错误正文不再由这些失败路径传到 renderer/model observation。配置与数据库 schema 不变。

## 验证

Skills parser 仅接受当前 DTO；路径错误、provider failure、Debug 脱敏均有针对性回归；运行 Rust workspace、UI、IPC drift 与安全相关门禁。

## 回滚

不需要持久数据重置。恢复 Skills 路径显示或 provider 错误正文传播前，必须先为新的数据披露契约写 ADR，并明确允许展示的字段和信任边界。
