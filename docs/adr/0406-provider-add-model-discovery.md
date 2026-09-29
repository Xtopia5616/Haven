# ADR 0406：Provider 添加时即时发现模型

- 状态：已采纳（2026-09-29）
- 范围：模型设置中的 Provider 添加、`discover_models` 可选鉴权参数与失败反馈
- 关联：ADR 0368（模型发现 command contract boundary）、ADR 0401（Windows Credential Manager credentials）

## 背景

Provider 在用户保存整个设置页前只存在于 renderer 草稿中。新增后立即拉取 `/models` 时，后端配置快照还没有该 Provider，无法从已保存配置确定 Anthropic、Gemini 或自定义 Header 的鉴权方式。若一律使用 Bearer，会把合法 Key 判成失败。

## 决定

1. 新 Provider 加入设置草稿后，立即调用现有 `discover_models` 拉取模型。显式输入的 Key 使用 Provider 预设传入的 Header 名和前缀；keyless 预设可明确要求不带认证 Header。
2. 空的模型响应也视为未拉取成功。成功时显示应用内成功通知；失败时保留已添加的 Provider，并以应用内弹窗提醒检查 Key、地址与预设，说明可以手动输入模型 ID。
3. 已保存 Key 的解析仍限制为 Provider 名称与规范化 Base URL 同时匹配。请求新增的鉴权参数只用于本次用户发起且显式提供 API Key 的发现请求，不改变配置持久化与 Credential Manager 写入流程。
4. 没有在线模型目录的 STT-only 协议仍可显示内置目录，但明确提示无法在线验证 API Key。

## 替代方案

- 等整个设置页保存后再发现：会失去添加 Provider 后的即时模型列表。
- 先把临时 Provider 或 Key 写入后端配置再验证：会提前持久化用户尚未提交的设置与凭据。
- 对所有预设都使用 Bearer：会破坏非 Bearer 协议的鉴权。

## 影响与重置

Tauri `discover_models` 增加可选 `auth_header_name`、`auth_header_prefix` 和 `skip_auth` 请求字段；既有调用无需传入。没有数据库或配置 schema 变化，也不需要重置用户数据。Key 仍只在设置页最终保存时进入 Credential Manager。

## 验证

实现后需运行 IPC contract 检查、Rust 格式/编译检查、前端类型检查与生产构建；本任务不改变 `ModelInfo` 响应 DTO。

## 回滚

回滚该功能代码与本 ADR 即可恢复现有添加和发现流程；无需配置或数据库迁移。
