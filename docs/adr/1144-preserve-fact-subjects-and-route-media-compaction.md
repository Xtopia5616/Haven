# 1144：保留多主体记忆事实并按媒体能力摘要

## 状态

已采纳并实施。

## 背景

Prompt 的 fact dedup key 缺少 subject，可能把不同项目/实体拥有相同 predicate/object 的事实折叠成一条。Compaction 按“含图片”与否选择 Chat/Vision，导致 video-only 内容经不支持视频的 Chat 映射丢失，audio-only 摘要也可能走不支持音频的路由。

## 决定

- Prompt 中 facts 的重复身份使用 `(subject, predicate, object)`，与 Memory SPO 语义一致。
- Compaction request kind 按完整输入 modality 选择：图片/视频使用 Vision，纯音频使用 AudioChat，无媒体使用 Chat。
- 请求类型选择集中在 compactor owner；既有 capability/route 失败处理仍负责生成稳定 fallback summary。

## 影响

相同关系的不同主体均可进入提示词；媒体摘要选择与主 ReAct 媒体能力路由一致。没有改变 transcript、fact schema 或 provider wire shape；无需数据重置。

## 验证

增加跨 subject prompt 回归和 audio/video compaction request kind 测试。运行 Rust workspace 测试和严格 Clippy。

## 回滚

无数据重置要求。回退 modality 路由前需证明所有当前支持 provider 在 Chat 路径都能保留/理解相同音频和视频内容。
