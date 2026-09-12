# ADR 0133：统一媒体与音频的模型工具契约

日期：2026-09-12
状态：已采纳

关联：[ADR 0130：媒体编排统一下沉到工具层](0130-media-orchestration-in-tool-layer.md)、
[ADR 0129：资产优先的模型媒体入口统一](0129-asset-first-model-media-entrypoints.md)

## 背景

当前模型目录同时暴露 `media` 和 `audio`。两者都采用 operation 分支、都经过同一安全网关，
且录音与媒体转写共享受管资产和 STT；但 `media` 这个名字却把音频设备能力排除在外，导致
工具目录、权限 key、UI renderer、能力快照和文档出现两套公共概念。音频的 Windows 设备副作用
确实需要独立的宿主实现，但这不要求继续保留独立的模型工具入口。

## 决定

1. 删除模型可见的独立 `audio` 工具。`media` 成为所有模型媒体能力的唯一公共入口。
2. `MediaOperation` 统一承载以下分支：

   | 类别 | operation | 输入边界 |
   |---|---|---|
   | 受管内容 | `inspect`, `describe`, `ocr`, `transcribe`, `extract` | 使用 `asset_id` |
   | 内容生成 | `generate` | 使用 `prompt` |
   | 音频设备/输出 | `record`, `play`, `speak`, `volume_get`, `volume_set`, `mute_get`, `mute_set` | 分支专属参数；`record` 产出受管 `asset_id`，`play` 仅接受受安全网关约束的 `.wav` 路径 |

3. 保留 `builtin/audio.rs` 作为 `AudioRuntime` 宿主适配层，只封装麦克风、WinMM、默认输出端点
   和 TTS 播放；它不实现 `Tool`，不注册工具，不拥有权限、schema 或结果 renderer。
4. `media` 的权限 key 按 operation 统一生成，例如 `media:record`、`media:speak`、
   `media:volume_set`；父级 `media` 仍可按现有 ancestry 规则继承。音频设备操作共享
   `media:audio-device` 的并发资源锁，避免录音、播放和端点设置互相竞态。
5. 录音先登记 WAV，再复用 `MediaTool::transcribe_asset`。STT 不可用时仍返回可复用资产和
   明确的不可用状态；这与文件和窗口 producer 的资产链保持一致。

## 影响与重置边界

- 模型调用从 `audio(operation=...)` 改为 `media(operation=...)`；不存在兼容 alias。
- UI 删除 `ToolAudioResult`，由 `ToolMediaResult` 渲染全部内容和音频设备结果。
- `[tool_settings.audio]`、旧的 `audio:*` 权限和旧未完成 audio tool call 不自动迁移。
  配置加载器会备份包含旧入口的配置并以默认值启动；旧快照按测试版策略不保证恢复。
- `media.audio` 仍保留为音频捕获的宿主配置区段；这是配置结构，不是第二个模型工具入口。
- 不修改数据库 schema；但升级后应删除旧运行态快照，必要时按 `docs/release-and-reset.md`
  删除数据库与媒体缓存。

## 替代方案

- 保留 `audio` 并仅在文档中解释其与 `media` 的关系：拒绝。公共入口仍重复，权限和 UI
  无法形成单一契约。
- 使用 `media.audio.*` 嵌套 operation：拒绝。当前内置工具采用单层 operation enum，
  与 `files` 的分支模型一致，也更适合 provider schema 和 permission key。
- 将 Windows 设备代码搬进 `media.rs`：拒绝。会把宿主 FFI 与受管资产编排耦合，扩大测试和
  平台条件编译边界。

## 验证与回滚

验证覆盖：工具注册表不再出现 `audio`；`media` schema 按录音/TTS 实时能力裁剪；媒体、音频
设备权限矩阵和 path sandbox 使用 `media`；录音资产仍可转写；UI media renderer 覆盖七个
音频设备分支。执行 `cargo fmt --all -- --check`、workspace check/test/clippy，以及 UI
check、test 和 production build。

若需要回滚，回退本 ADR 对应提交并删除新版本产生的 `media` 运行态快照；不要把新旧
`audio`/`media` tool call 混在同一 ReAct snapshot 中。
