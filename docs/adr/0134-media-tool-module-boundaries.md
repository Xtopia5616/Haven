# ADR 0134：媒体工具按职责拆分内部模块

日期：2026-09-12
状态：已采纳

关联：[ADR 0133：统一媒体与音频的模型工具契约](0133-unified-media-audio-tool-contract.md)、
[ADR 0129：资产优先的模型媒体入口统一](0129-asset-first-model-media-entrypoints.md)

## 背景

`builtin/media.rs` 已经承载模型工具契约、能力裁剪、媒体引用、图片派生、音频转写、文档抽取、
图片生成、资产登记和测试。虽然对外已经统一为 `media`，但单文件仍混合多个变化原因，导致
修改一个 operation 时需要阅读整份文件，也无法形成清晰的直接模块边界。

## 决定

保留一个公共 `media` 工具入口，但按职责拆分实现：

| 模块 | 权责 |
|---|---|
| `media.rs` | `MediaTool`、`MediaOperation`、`MediaParams`、schema、权限/风险/并发/超时和统一调度 |
| `media_reference.rs` | 媒体模态分类、模型媒体引用、operation 名称和通用文本/置信度辅助函数 |
| `media_content.rs` | 图片描述/OCR、音频转写、文档抽取、受限读取和失败/取消结果 |
| `media_generation.rs` | 图片生成以及生成/路径媒体的受管资产登记 |
| `media_tests.rs` | `MediaTool` 的契约、资产引用、provider fallback 和取消行为测试 |
| `media_audio.rs` | 统一 `media` 工具中的音频 operation 分发，以及麦克风、播放、TTS、音量和静音的宿主设备适配；不实现 `Tool` |

模块之间依赖单向：公共契约调度各能力模块，能力模块复用引用/资产接口；任何模块都不能新增
第二个模型工具入口。对外工具名、operation、权限 key、结果 JSON 和配置不变。

## 拒绝的替代方案

- 继续把所有实现留在 `media.rs`：拒绝，单文件同时承担超过两个独立职责，违反热点拆分预算。
- 把每个 operation 做成独立的模型工具：拒绝，会重新产生工具目录、权限和 UI 契约碎片。
- 把 Windows 音频设备适配移入媒体内容模块：拒绝，平台 FFI 应继续处于最小宿主边界。

## 影响与重置边界

- 不改变模型可见工具、输入 schema、权限 key、结果 shape、配置或数据库 schema。
- 不需要数据重置；现有 `media` 运行态和受管资产生命周期保持不变。
- 新增媒体能力应优先进入对应职责模块，并在 `media.rs` 只增加 operation 契约和调度连接。

## 验证与回滚

验证：`cargo fmt --all -- --check`、`cargo check --locked -p haven-tools`、
`cargo test --locked -p haven-tools`，并执行工作区完整测试与严格 Clippy。

回滚：回退本 ADR 对应提交即可；不需要转换配置、数据库或媒体资产。
