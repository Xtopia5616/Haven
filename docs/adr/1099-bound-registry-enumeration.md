# ADR 1099：限制并隔离 Windows Registry 枚举

## 状态

已接受（2026-10-13）

## 背景

Registry `list` 同步枚举所有 value 和 subkey，并为只显示 value name 的路径加载完整 value data。大键会阻塞 Tokio worker 或产生不受控的输出与分配；取消 token 也没有传入枚举循环。

## 决定

- 同步 Windows Registry 调用在 `spawn_blocking` 中执行；调用方 future 等待时同时监听取消，后台枚举在每个 Registry API 调用之间检查 token。
- `list` 对 values 和 subkeys 分别最多收集 256 个名称；超过上限时通过 `truncated.values` 与 `truncated.subkeys` 明确报告。
- value 名使用 names-only `RegEnumValueW`，数据缓冲指针为空；`get` 继续读取值数据并保留既有输出形状。

## 影响

取消能让工具调用方立即返回，但不能中止一个已经进入 Windows Registry API 的单次同步调用；阻塞线程会在 API 返回后检查 token 并退出。大键结果可能不完整，调用者需检查 `truncated`。

## 验证

加入不依赖真实 Registry 的限额、截断、取消和 GET 输出形状回归。Windows API 真实 Registry 行为仍须由 Windows 构建与发布验收确认。

## 回滚

不得恢复全量 value data 读取或在 Tokio worker 上同步遍历。若提高限额，应基于响应预算并继续显式报告截断。
