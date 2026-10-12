# ADR 1102：终止事件时关闭录音与 provider stream reader

## 状态

已接受（2026-10-13）

## 背景

Input 的 normal VAD worker 在错误路径返回前没有发送 auto-stop，可能让 App recording lifecycle 留在活动状态。LLM line reader 在 provider body 静默时只等待下一块数据；如果下游 stream 被丢弃，HTTP body reader 仍可能持有连接。

## 决定

- Normal 模式 VAD worker 失败时先通过既有 auto-stop handler 结束 recording lifecycle，再返回保留 capture error 的结果；PCM 模式、manual stop 和 duration stop 保持原行为。
- LLM reader 同时等待下一块 body 数据与发送端关闭；发送端关闭后尽快退出并释放 body。调用点仍保持 detached reader 行为，取消生命周期由 channel close 表达。

## 影响

输入错误不会遗留活动录音；下游放弃 provider stream 时可以释放挂起的 body reader。现有事件、错误类型和内容契约不变。

## 验证

Input 的硬件无关回归覆盖错误通知和 capture error；LLM stream 测试使用永不产生数据的 body 与 DropProbe 验证 receiver close 后 body 被释放，并复核 backpressure 路径。

## 回滚

不得移除 VAD error auto-stop 或恢复只等待 body 的 reader。若改变生命周期，应明确关闭路径和 body owner，并保留无硬件/无网络 fixture 的回归。
