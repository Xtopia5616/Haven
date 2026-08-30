# ADR 0015：Linux 适配的宿主边界与验收路线

## 状态

暂缓，不计入 P2 完成条件。本文记录后续独立阶段/版本的 Linux 与跨平台适配工作，不表示相关实现已经完成。

## 背景

Haven 当前以 Windows 桌面应用为主要目标，但核心 Rust workspace 已经在
Ubuntu 与 Windows 上执行 Rust 测试，UI 也在两个平台上执行检查、测试和生产构建。
仍未完成的是 Linux 桌面运行时、音频设备差异、进程生命周期以及 Windows 宿主能力的
跨平台边界收敛。

本 ADR 承接 Linux 适配前四项工作之后的剩余事项，重点覆盖音频实时性、tract CPU
回归、子进程取消、能力降级、通知/自启、Wayland 限制与发布验收。

## 决定

### 1. 保持核心契约平台无关

- `haven-agent`、`haven-llm`、`haven-memory` 以及工具的安全策略不感知具体桌面平台。
- `haven-common` 只放稳定 DTO、配置模型、纯函数和平台能力描述，不放 Tauri、窗口系统
  或音频驱动逻辑。
- 平台差异收敛在 `haven-input` 的采集适配器、`haven-tools` 的进程/系统适配器和
  `haven-app-binary` 的桌面宿主适配器中；业务调用方只依赖稳定接口。
- 适配器的失败必须表达为可诊断的错误或能力不可用状态，不能通过静默 fallback 或
  `panic` 制造跨平台行为差异。

### 2. 音频回调保持实时线程不变量

音频管线的稳定输出契约仍为单声道、16 kHz、`f32`。CPAL 回调线程只负责采样格式转换、
重采样和投递，不负责模型推理、磁盘写入、网络请求或阻塞式日志。

后续优化按以下顺序执行：

1. 在协商到 stream 配置后预分配 mixdown、resampler scratch 和必要的环形缓冲空间；
2. 消除回调线程中的动态扩容、不可控日志和长时间 mutex 等待；
3. 在行为测试覆盖后，再评估有界 SPSC ring，保持溢出策略和取消语义不变；
4. 为 44.1/48/96 kHz、单声道/立体声、`f32`/整数采样格式以及不规则 callback
   chunk 增加契约测试。

设备适配器必须在每次打开 stream 时重新解析实际 host、设备和配置；设备名称只能作为
   展示信息，稳定设备标识优先。设备拔出、默认设备变化和 stream error 应进入明确的
   可恢复或需用户重试状态，并记录实际设备诊断信息。

### 3. tract 与 Silero state 继续分层

- 业务代码只通过 `tract` facade 使用 `Runnable`、`State` 和 `Tensor`，不得重新依赖
  `tract-core`、`tract-onnx` 或 `TypedSimplePlan` 等内部类型。
- 一个 VAD worker 独占一个 tract execution `State`，连续帧复用该 state，不在线程间共享。
- Silero v5 的 recurrent tensor 是模型图的显式输入/输出，必须与 tract execution
  state 分开保存。
- 每次录音开始前必须同时重置两类 state；reset 的顺序需要由串行 worker channel
  保证，并补充 reset 后首帧与新 engine 的回归测试。
- Linux 验收以 CPU 推理为基线，必须有不依赖本机 CUDA 安装的模型加载、连续推理和
  reset smoke test。若后续需要缩减二进制，再单独评估 facade feature，不以重新引入
  tract 内部 crate 作为解决方案。

### 4. 子进程取消按进程组定义

Shell 工具的取消、超时和后台迁移必须定义为“终止本次命令创建的进程组”，而不是只
终止直接子进程。Windows 保持现有平台实现，Unix/Linux 使用对应的 process group
机制；两端都必须覆盖：

- 前台取消；
- 超时后转后台；
- shell 派生孙进程；
- 关闭应用后的清理；
- 进程组创建失败时的安全降级。

所有路径仍必须经过现有安全网关、风险分级和取消边界。

### 5. Windows 专属能力以 capability 表达

注册表、Windows UI 自动化、任意窗口控制、输入模拟、Windows 音频控制和部分电源
操作不应被伪装成 Linux 等价能力。工具或系统诊断应能返回能力是否可用、受限原因和
必要的用户操作。

通知、自启和全局快捷键分别使用桌面适配器：

- 原有 `notification.*.windows` 配置在迁移前保留读取兼容；新的公共语义应逐步改为
  `native`/`desktop`，避免把 Windows 名称暴露为跨平台契约；
- Linux 自启在实现时选择 XDG autostart 或 systemd user service，并补安装、更新、
  删除和旧路径清理测试；未实现前继续明确返回“不支持”，不得假装成功；
- X11 与 Wayland 分开验收。Wayland 下全局快捷键、任意窗口控制、截图和 UI 自动化
  可能受 compositor 或 portal 限制，产品契约必须允许 `limited`/`unsupported`。

### 6. CI、桌面构建和发布分层验收

现有 Ubuntu/Windows Rust 测试和 UI 门禁继续保留。新增 Linux 支持时，按以下层次增加
验收：

1. Ubuntu 上的 workspace check、clippy、测试和 CPU VAD smoke；
2. Ubuntu 上的 Tauri 桌面构建 smoke，验证 WebKitGTK/GTK、`pkg-config` 和 CPAL
   系统依赖；
3. 至少一次 X11 和一次 Wayland 桌面启动验证；
4. 选择明确发行格式（例如 deb 或 AppImage）后，增加安装、升级、卸载和用户数据
   保留测试；
5. README 记录发行版依赖、音频后端限制、通知/自启能力和已知 Wayland 差异。

## 影响与替代方案

该方案优先保护现有 Windows 行为和 Rust 核心契约，代价是 Linux 首版不会承诺所有
Windows 工具能力等价，也不会把 X11 与 Wayland 混为一个“Linux 支持”开关。

不采用以下方案：

- 在业务层增加大量 `cfg(windows)`/`cfg(unix)` 分支；这会使 IPC、工具安全策略和
  失败语义继续漂移；
- 为了 Linux 直接绑定某个具体 ALSA/Pulse/PipeWire 实现；先通过 CPAL 适配器和设备
  诊断验证实际需求，再决定发行版依赖；
- 为了获得 tract 类型而重新直接依赖内部 crate；公共 API 仍以 facade 为唯一入口；
- 为 Wayland 缺失的能力返回空成功结果；不可用能力必须可观测并对用户可见。

## 验证

实现每个切片时至少补充对应测试，并执行：

```text
cargo fmt --all -- --check
cargo check --workspace --locked
cargo clippy --workspace --locked -- -D warnings
cargo test --workspace --locked -- --test-threads=1
corepack pnpm --dir ui run check
corepack pnpm --dir ui run test:run
```

涉及桌面宿主或音频设备时，另行执行 Linux 原生桌面构建和真实设备验收；硬件依赖
测试不得替代无硬件的契约测试。

## 回滚

本文为规划文档，不产生运行时迁移。若后续实现需要回滚，按切片回退对应平台适配器、
配置迁移和发布脚本；不得通过删除用户数据目录来掩盖路径或配置回滚问题。任何破坏
已有配置、数据库或 IPC 契约的实现必须另附迁移与重置说明。
