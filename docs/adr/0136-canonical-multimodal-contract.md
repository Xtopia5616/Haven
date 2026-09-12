# ADR 0136：多模态探测、表示与结果契约统一

日期：2026-09-12
状态：已采纳

关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)、
[ADR 0129：资产优先的模型媒体入口统一](0129-asset-first-model-media-entrypoints.md)、
[ADR 0133：统一媒体与音频的模型工具契约](0133-unified-media-audio-tool-contract.md)、
[ADR 0134：媒体工具按职责拆分内部模块](0134-media-tool-module-boundaries.md)

## 背景

图片、音频、视频和文档已经共享 `MediaAsset` 与 `MediaPlan`，但文件分类、STT fallback、
工具 JSON 和 UI 仍存在重复实现。视频在 common/LLM 的底层类型中存在，却没有稳定地经过
files、media、Agent 恢复和 UI。`media` 还同时承载受管资产处理与音频设备副作用。

## 决定

1. `haven-common::media_detection` 是 MIME/扩展名/魔数到 `MediaType` 的唯一权威探测器；
   LLM 媒体模块不再维护第二份 detector，files、app ingress 和 tools 均调用 common。
2. `MediaRepresentationKind` 是跨 common、tool JSON、Agent notice 和 UI 的表示枚举。工具结果
   统一使用 `MediaResult` 外壳与 `MediaReference`，operation-specific 字段只能作为外层扩展；
   不再引入 `generated_image` 等未登记字符串。
3. `RawVideo` 贯通 attachment、files handoff、Agent snapshot/role selection、projection 与
   UI preview；planner 只在 provider capability 明确支持时发送，unsupported adapter 必须显式
   降级/报错。
4. LLM chat STT fallback 复用 `MediaInput → MediaPlan → project_media_plan`，不再单独构造
   `ContentPart::Audio`。
5. `MediaOperation` 对外保持兼容的扁平 schema，内部按 `MediaAssetOperation` 与
   `AudioDeviceOperation` 分组；`record` 归资产操作但额外占用音频设备资源。
6. UI voice input 与 `media.record` 保持两个有意的生命周期：前者产生纯文本消息，后者产生
   可复用的 WAV asset，并通过统一 media result 返回 transcript。

## 替代方案

- 继续在每个 crate 维护局部扩展名表：拒绝，会让同一文件在 ingress/files/LLM 得到不同 MIME。
- 为 generated image 新增仅工具层字符串：拒绝，表示必须先进入 common 枚举。
- 把 video 无条件发送给所有 provider：拒绝，能力未知或不支持时会产生错误/占位内容。
- 把 `audio` 设备操作拆成第二个模型工具：拒绝，公共入口和权限/renderer 会再次分裂。

## 影响与重置边界

新增 common 探测器、typed media result、视频预览和内部模块；既有 `media` operation 名称保持不变。
`play` 结果不再返回宿主路径，只返回是否已交给扬声器。没有数据库 schema 变更；旧快照按现有
测试版 reset 边界处理，新的 media result 不要求迁移历史 tool observation。

## 验证与回滚

验证 common/LLM/tools 编译、media/file/app 回归测试、UI check/test/build，以及 workspace
format/check/test/clippy。回滚时回退本 ADR 对应提交；若运行态快照来自新旧 media result
混合版本，按 `docs/release-and-reset.md` 删除快照/媒体缓存，不做猜测式迁移。
