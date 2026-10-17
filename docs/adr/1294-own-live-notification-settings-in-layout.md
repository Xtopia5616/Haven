# ADR 1294：由 layout 拥有通知设置运行快照

状态：已接受并实施。

## 背景

通知设置保存后，Settings 页只更新 ToolRun completion 的独立 UI store；会话、权限和其它通知由 `+layout.svelte` 内的 `notifyCfg` 控制。这样同一份持久配置在应用运行期间可能出现两种版本，且不同通知类别的行为不一致。

## 决定

1. `+layout.svelte` 是 renderer 内所有通知类别的唯一运行时配置投影 owner。ToolRun completion 与会话、权限等通知都从 layout 当前快照读取应用内渠道设置。
2. Settings 不持有或更新第二份运行时投影。配置命令成功后，Settings 通过同窗口的 typed notification-settings event 发布已保存快照；layout 合并该快照。
3. 普通持久化失败不发布变更。若配置已 durable-first 写入、后续 runtime apply 失败，Settings 重新读取权威配置，再发布已落盘的通知部分；读取失败时保留 layout 现值并报告错误。
4. layout 初始 settings 请求仍不阻塞事件 listener 安装。保存事件带来的更新会增加本地 revision；初始请求只有在期间没有收到新快照时才能写入，避免晚到旧读取覆盖新配置。
5. 事件只在当前窗口进程内传递配置快照，不增加持久字段、Rust IPC 命令或新的全局状态 store。

## 验收

- Settings 保存成功后发布当前 notification snapshot。
- durable-first partial-apply 失败后读取已写配置并发布；普通保存失败不应用草稿值。
- layout 的 ToolRun completion 应用内提示与其它通知类别使用同一份快照。
- 初始异步读取不能覆盖其启动后收到的保存快照。
- 不改变通知设置持久格式或数据库，无需数据重置。
