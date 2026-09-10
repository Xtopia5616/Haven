# ADR 0116：多模态请求与受管上传边界加固

日期：2026-09-10
状态：已采纳
关联：[ADR 0113：统一多模态资产、表示与请求投影](0113-unified-media-asset-representation-projection.md)、
[ADR 0114：受限本地文档抽取与派生表示](0114-bounded-document-extraction.md)、
[ADR 0115：受管上传资产生命周期与清理](0115-managed-upload-lifecycle-and-cleanup.md)

## 背景

首轮多模态切片已经建立了 `asset_id`、受管文件工具、文档抽取和 provider
capability profile，但 host 边界、失败回滚、生命周期引用保护和请求投影仍有
高风险缺口：renderer 可携带历史 `path`，上传失败可能留下临时文件，活动会话
引用可能被 TTL 清理，PDF Flate/Office XML 解析也缺少完整的输出和失败上限。

## 决定

- `validate_attachments` 和持久化入口同时清空 renderer 的 `asset_id` 与 `path`。
  注册表只接受专用 uploads 根目录下的普通文件，并拒绝符号链接、Windows
  reparse point、相对逃逸和根目录外路径。
- 普通文件先写入 `.file-{uuid32}.tmp`，成功后原子改名为 `file-{uuid32}`；失败、
  取消或任务丢弃时删除 staging 目录。`context_limits.max_upload_total_bytes`
  作为 uploads 总容量上限，文件名冲突按 Windows 大小写不敏感规则去重。
- 清理先从持久化消息构建引用集合，保护活动/暂停会话仍引用的资产；随后 prune
  已从历史删除的 registry 映射，并在目录删除或缺失后再次清理无效映射。
- ReAct 先按 canonical media requirements 选择目标 role，再读取该 adapter 的
  `capability_profile()` 重建 provider request projection。降级/省略生成结构化
  `agent:media_plan` 事件，并由 UI 展示稳定的原因码；canonical transcript 不被
  provider 能力污染。
- PDF 单 stream 与总解压输出有硬上限，Office shared strings 计入总额度；PDF/Office
  XML 解析错误 fail-closed，并在文档循环和解压读取中响应 cancellation token。

## 替代方案与影响

保留 renderer path、仅依赖 ingress 校验、或继续用固定的“图片/音频支持”假设都
无法覆盖历史消息、不同 adapter 和进程内工具调用。新增的 registry 重校验、临时
目录、容量统计和 provider request copy 会增加少量 I/O/CPU，但不把绝对路径或原始
附件 metadata 写入模型上下文；旧进程 registry 在重启后自然失效，历史附件需要
重新上传。

## 验证与回滚

回归测试覆盖 renderer/history path 清除、根目录外注册拒绝、上传回滚和总配额、
活动引用保护、registry prune、Windows 大小写冲突、PDF 解压炸弹、Office XML
解析失败、取消，以及 capability notice 的 Rust/UI wire 映射。回滚只需回退本次
代码和配置字段；无数据库 schema 变化，旧 uploads 可人工清理。
