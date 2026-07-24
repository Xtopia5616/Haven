# Haven — 详细设计文档

> 版本: v1.0 | 日期: 2026-07-19 | 基于 `docs/summary.md` 需求澄清后生成

---

## 目录

1. [项目概述](#1-项目概述)
2. [架构设计](#2-架构设计)
3. [交互设计](#3-交互设计)
4. [模块详细设计](#4-模块详细设计)
   - [4.1 desktop](#41-desktop)
   - [4.2 app-binary (Tauri Bridge)](#42-app-binary-tauri-bridge)
   - [4.3 input](#43-input)
   - [4.4 agent](#44-agent)
   - [4.5 task](#45-task)
   - [4.6 tools](#46-tools)
   - [4.7 memory](#47-memory)
   - [4.8 notification](#48-notification)
5. [UI 设计](#5-ui-设计)
6. [数据模型](#6-数据模型)
7. [安全设计](#7-安全设计)
8. [开发阶段与 Crate 拆分](#8-开发阶段与-crate-拆分)
9. [附录](#9-附录)

---

## 1. 项目概述

### 1.1 项目定位

Haven 是一个基于 Pi Coding Agent (ReAct 循环) 架构的 Windows PC 语音助手。用户通过热键唤醒、语音输入指令，助手在本地完成语音转写后调用 LLM 进行意图分类与任务推理，通过 MCP/Skills/内置工具执行操作，最终在 UI 和系统通知中输出结果。

### 1.2 核心原则

| 原则 | 说明 |
|------|------|
| 隐私优先 | 音频仅本机处理，不上传云端 |
| 工具驱动 | 键鼠/窗口控制不内置，全部通过 MCP 接入 |
| 可审计 | 全量工具调用链可追溯 |
| 安全确认 | 高危操作弹窗确认，MCP 工具默认需确认 |
| 首发 Windows | 架构预留跨平台扩展点 |

### 1.3 技术栈

| 层 | 技术选型 |
|----|----------|
| 后端 | Rust (workspace 多 crate) |
| 前端 | Tauri v2 + SvelteKit SPA |
| 持久化 | SQLite (多文件拆分) |
| 音频捕获 | CPAL |
| VAD | Rust 原生 Silero VAD (移植) |
| STT | MCP 协议 (用户自配 STT 服务器) |
| LLM | 云端优先，本地备选；classifier / reasoner / fallback 三模型路由 |
| 热键 | Tauri global shortcut API |
| 托盘 | Tauri tray API |
| 开机自启 | Windows Task Scheduler |

### 1.4 应用生命周期

1. 应用启动后先初始化配置、日志、数据库连接与托盘图标。
2. 再注册全局热键与开机自启状态。
3. 然后恢复最近会话、待处理任务和偏好记忆。
4. 最后才进入可用状态，接收语音与任务输入。
5. 退出时先停止录音与任务调度，再关闭 MCP 连接与数据库，最后退出进程。

---

## 2. 架构设计

### 2.1 总体架构图

```
┌─────────────────────────────────────────────────────────────┐
│  desktop (托盘/热键/通知/开机自启)                      │
│  ┌───────────────────────────────────────────────────────┐  │
│  │  app-binary (Tauri invoke/emit 桥)                    │  │
│  │  ┌─────────────────────────────────────────────────┐  │  │
│  │  │  agent (LLM 编排 / ReAct 循环)            │  │  │
│  │  │  ┌──────────────────────┐ ┌──────────────────┐ │  │  │
│  │  │  │ task        │ │ input   │ │  │  │
│  │  │  │ (任务队列/并发调度)   │ │ (录音/VAD/缓冲)  │ │  │  │
│  │  │  └────────┬─────────────┘ └──────────────────┘ │  │  │
│  │  │           │                                     │  │  │
│  │  │  ┌────────┴──────────────────────────────┐      │  │  │
│  │  │  │ tools                                 │      │  │  │
│  │  │  │ ├─ MCP 客户端 (外部工具)              │      │  │  │
│  │  │  │ ├─ Skills 引擎 (本地脚本)             │      │  │  │
│  │  │  │ ├─ 内置工具 (文件/进程/剪贴板)        │      │  │  │
│  │  │  │ └─ 安全确认网关                       │      │  │  │
│  │  │  └───────────────────────────────────────┘      │  │  │
│  │  │  ┌───────────────────────────────────────┐      │  │  │
│  │  │  │ memory (SQLite 多文件持久化)          │      │  │  │
│  │  │  └───────────────────────────────────────┘      │  │  │
│  │  └─────────────────────────────────────────────────┘  │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 Agent 核心: ReAct 循环

Pi Coding Agent 的 ReAct 循环是系统核心推理引擎，每个任务执行遵循:

```
┌──────────┐     ┌──────────┐     ┌──────────┐
│ Thought  │ ──▶ │  Action  │ ──▶ │Observe   │ ──┐
│ (推理)   │     │ (行动)   │     │ (观察)   │   │
└──────────┘     └──────────┘     └──────────┘   │
      ▲                                           │
      └───────────────────────────────────────────┘
                    (循环直到任务完成)
```

- **Thought**: LLM 分析当前状态、决定下一步行动
- **Action**: 调用工具 (MCP/Skills/内置工具) 或返回最终答案
- **Observation**: 工具执行结果作为下一轮输入

### 2.3 Crate 拆分

```
crates/
├── haven-desktop/       # 桌面壳: 托盘、热键、开机自启、通知
├── haven-bridge/        # Tauri invoke/emit 桥接层
├── haven-agent/         # Agent 核心: ReAct 循环、LLM 路由、意图分类
├── haven-task/          # 任务队列、并发调度、打断/恢复
├── haven-input/         # CPAL 录音、Silero VAD、音频缓冲
├── haven-tools/         # MCP 客户端、Skills 引擎、内置工具
├── haven-memory/        # SQLite 持久化 (会话/偏好/历史/事实)
├── haven-common/        # 共享类型、错误定义、配置模型
└── haven-llm/           # LLM 客户端抽象 (classifier/reasoner/fallback)
```

### 2.4 数据流

```
用户语音 --> [热键触发] --> [CPAL 录音] --> [VAD 切分]
                                              |
                                              v
                                     [音频缓冲 WAV]
                                              |
                                              v
                                     [MCP STT 转写] --> 文本
                                              |
                                              v
                               [agent: classifier 分类]
                                     |          |
                               "新任务"    "补充当前上下文"
                                     |          |
                               ┌─────┘          └──────┐
                               v                       v
                        [task           [追加到当前
                         新建任务]               任务上下文]
                               |
                               v
                        [ReAct 循环]
                          |- Thought (reasoner)
                          |- Action (工具调用)
                          |   |- MCP 工具 --> 安全确认网关
                          |   |- Skills --> 安全确认网关
                          |   `- 内置工具 --> 安全确认网关
                          `- Observation
                               |
                               v
                        [UI 更新 + 通知]
```

---

## 3. 交互设计

### 3.1 热键

| 项目 | 配置 |
|------|------|
| 默认模式 | **按一下切换**（toggle），按一次开始录音，再按一次结束 |
| 替代模式 | **按住说话**（PTT），按住录音，松开结束 |
| 切换方式 | 设置页面切换 |
| 默认快捷键 | `Ctrl+Shift+Space` |
| 自定义 | 支持用户在设置中重新绑定热键 |

### 3.2 语音输入流程

```
用户按下热键 ──▶ 开始录音 ──▶ VAD 检测语音活动
                                  │
                    ┌─────────────┴─────────────┐
                    ▼                           ▼
              检测到语音                     静音超时 (1.5s)
                    │                           │
                    ▼                           ▼
              继续录音                     自动结束录音
                    │                           │
                    └───────────┬───────────────┘
                                ▼
                         生成 WAV 文件
                                │
                                ▼
                         MCP STT 转写
                                │
                                ▼
                         文本送入 agent
```

- **静音超时**: 1.5 秒 (可配置)
- **最大录音时长**: 60 秒 (可配置)

### 3.3 输出方式

| 输出类型 | 渠道 | 说明 |
|----------|------|------|
| 任务状态 | UI 主面板 | 任务队列、当前执行步骤 |
| 转写结果 | UI 主面板 | 实时或最终转写文本 |
| 执行结果 | UI 主面板 + 通知 | 结构化结果展示 |
| 确认弹窗 | 系统对话框 | 高危操作确认 |
| 错误/告警 | 系统通知 | 原生 Windows toast |

**不实现语音播报。**

---

## 4. 模块详细设计

---

### 4.1 desktop

**Crate**: `haven-desktop`

#### 4.1.1 系统托盘

左键点击显示或隐藏主窗口。右键菜单项：

| 菜单项 | 功能 |
|--------|------|
| 显示主窗口 | 打开/聚焦 SvelteKit UI |
| 静音模式 | 切换：停止响应热键、不录音 |
| 设置 | 打开设置页面 |
| 退出 | 关闭所有任务、退出进程 |

托盘图标状态:
- **正常**: 默认图标 (蓝色)
- **录音中**: 红色圆点/动画
- **静音**: 灰色 + 斜杠
- **忙碌**: 橙色 (任务执行中)

#### 4.1.2 热键注册

- 通过 Tauri global shortcut API 注册全局热键
- 静音模式下不触发录音
- 支持运行时重新绑定

#### 4.1.3 开机自启

- 使用 Windows Task Scheduler 注册任务
- 触发器: 用户登录时
- 设置页面提供开关

#### 4.1.4 通知

- 通过 Tauri notification API 调用 Windows 原生 toast
- 通知类型: 任务完成、错误告警、确认请求

---

### 4.2 app-binary (Tauri Bridge)

**Crate**: `haven-bridge`

#### 职责

Tauri Rust 后端与 SvelteKit 前端的桥接层。所有跨边界通信通过 `invoke` (前端→后端) 和 `emit` (后端→前端) 实现。

#### invoke 命令

```rust
// 录音控制
#[tauri::command] fn start_recording() -> Result<RecordingHandle>
#[tauri::command] fn stop_recording() -> Result<AudioBuffer>
#[tauri::command] fn cancel_recording()

// 任务控制
#[tauri::command] fn submit_task(text: String) -> Result<TaskId>
#[tauri::command] fn cancel_task(task_id: TaskId)
#[tauri::command] fn pause_task(task_id: TaskId)
#[tauri::command] fn resume_task(task_id: TaskId)

// 工具管理
#[tauri::command] fn list_mcp_tools() -> Vec<McpToolInfo>
#[tauri::command] fn configure_mcp(config: McpConfig)
#[tauri::command] fn list_skills() -> Vec<SkillInfo>
#[tauri::command] fn refresh_skills()

// 记忆查询
#[tauri::command] fn search_history(query: String) -> Vec<TaskRecord>
#[tauri::command] fn get_conversation_memory() -> Vec<Message>
#[tauri::command] fn update_preference(key: String, value: String)

// 确认弹窗
#[tauri::command] fn resolve_confirmation(confirm_id: ConfirmId, approved: bool)

// 配置管理
#[tauri::command] fn get_settings() -> Settings
#[tauri::command] fn update_settings(settings: Settings)
```

#### emit 事件

```rust
// 录音状态
emit("recording:started")
emit("recording:stopped", AudioBuffer)
emit("recording:vad_status", bool)  // 是否检测到语音

// 转写
emit("transcription:result", { task_id, text })

// 任务状态
emit("task:created", TaskInfo)
emit("task:updated", TaskInfo)
emit("task:completed", TaskResult)
emit("task:error", TaskError)

// Agent 推理 (流式)
emit("agent:thought", { task_id, thought })
emit("agent:action", { task_id, action })
emit("agent:observation", { task_id, observation })

// 确认请求
emit("confirm:requested", ConfirmRequest)

// 通知
emit("notify", Notification)
```

---

### 4.3 input

**Crate**: `haven-input`

#### 4.3.1 音频捕获

- **设备**: CPAL 获取系统默认输入设备，支持设备热插拔时自动重连到新的默认输入设备
- **格式**: WAV, 16kHz, 16-bit, mono
- **缓冲**: 环形缓冲区 (ring buffer)，默认 5 秒容量

#### 4.3.2 VAD (Silero VAD)

- **引擎**: 将 Silero VAD ONNX 模型通过 `ort` (ONNX Runtime) 在 Rust 中运行
- **帧大小**: 30ms (480 samples @ 16kHz)
- **阈值**: 语音概率 > 0.5 判定为语音帧
- **静音容忍**: 连续 50 帧 (1.5s) 无声则自动结束

#### 4.3.3 STT 调用

- **方式**: 通过 MCP 协议调用用户配置的 STT 服务器
- **协议**: 发送 WAV 音频，接收转写文本
- **超时**: 30 秒
- **失败处理**: UI 提示 "转写失败，请检查 STT 服务配置"
- **备选路径**: 自定义 LLM STT 适配器，作为 MCP 方案不可用时的替代实现

#### 4.3.4 数据模型

```rust
struct AudioConfig {
    sample_rate: u32,        // 16000
    channels: u16,           // 1
    bits_per_sample: u16,    // 16
    max_duration_secs: u64,  // 60
    silence_timeout_ms: u64, // 1500
    vad_threshold: f32,      // 0.5
}

struct RecordingSession {
    id: SessionId,
    started_at: Instant,
    buffer: RingBuffer<i16>,
    vad_state: VadState,
}

enum VadState {
    Silent,
    Speech,
    SilenceAfterSpeech { silent_frames: u32 },
}
```

---

### 4.4 agent

**Crate**: `haven-agent`

#### 4.4.1 LLM 路由

三种模型角色，通过 `haven-llm` 客户端抽象统一调用:

| 角色 | 职责 | 触发条件 |
|------|------|----------|
| **Classifier** | 意图分类、工具路由选择、判断"新任务/补充当前上下文" | 每次新输入 |
| **Reasoner** | ReAct 循环中的 Thought/Action 推理，是主工作模型 | 任务执行全程 |
| **Fallback** | 降级处理 | Reasoner 不可用 (超时/错误/额度耗尽) 时自动切换 |

**配置模型**:

```rust
struct LlmConfig {
    classifier: ModelEndpoint,
    reasoner: ModelEndpoint,
    fallback: ModelEndpoint,
}

struct ModelEndpoint {
    provider: String,
    base_url: String,
    api_key: String,        // 加密存储
    model_name: String,
    max_tokens: u32,
    temperature: f32,
    timeout_secs: u64,
}
```

每个模型可配置独立的 base_url / api_key / model_name。

#### 4.4.2 意图分类 (Classifier)

输入为转写文本 + 当前任务上下文，输出:

```rust
enum Classification {
    NewTask {
        summary: String,
        priority: TaskPriority,
        suggested_tools: Vec<ToolName>,
    },
    AppendToTask {
        task_id: TaskId,
        additional_context: String,
    },
}
```

优先级:

```rust
enum TaskPriority {
    Low,      // 低优先级，排队末尾
    Normal,   // 默认
    High,     // 插队到队列头部
    Critical, // 中断当前任务立即执行
}
```

#### 4.4.3 ReAct 循环 (Reasoner)

```
Loop:
  1. 构建 Prompt (system + 任务上下文 + 工具描述 + 历史 Thought/Action/Observation)
  2. 调用 Reasoner LLM
  3. 解析输出:
     - Thought: 推理文本，流式推送到 UI
     - Action: 函数调用 (tool_name + params) 或 FinalAnswer
  4. 执行 Action:
     - 工具调用 -> MCP/Skills/内置工具 -> 安全网关
     - 无可用工具时 -> 直接生成自然语言答复
     - FinalAnswer -> 结束循环
  5. 将 Observation 追加到上下文
  6. 判断终止条件:
     - FinalAnswer
     - 达到 max_steps (默认 30)
     - 用户取消
```

**Prompt 结构**:

```
System: 你是 Haven PC 语音助手...
Available Tools: [工具描述列表]
Current Task: {任务描述}
Conversation History: [历史消息]
Previous Steps: [Thought → Action → Observation 序列]
```

#### 4.4.4 Fallback 策略

当 Reasoner 出现以下情况时自动切换:
- HTTP 超时 (默认 60s)
- 5xx 错误
- API key 无效
- Rate limit 超限

切换后:
1. 向 UI 发送通知 "主模型不可用，已切换至备用模型"
2. 使用 fallback 模型继续推理
3. 如果 fallback 也不可用，则退回到最小可用响应模式，提示用户稍后重试
4. 下一个新任务恢复使用 reasoner (可配置)

---

### 4.5 task

**Crate**: `haven-task`

#### 4.5.1 任务状态机

```
          |        |
          |        |
          |--> Running --> (取消) ---------------------> Cancelled
          |        |
          |        +--> Paused --> (取消) -----------> Cancelled
          |
          +--> Pending --> (取消) ------------------> Cancelled
```

状态:

```rust
enum TaskStatus {
    Pending,     // 排队中
    Running,     // 执行中 (ReAct 循环中)
    Paused,      // 用户暂停
    Completed,   // 正常完成
    Cancelled,   // 用户取消
    Error,       // 执行错误
}

struct Task {
    id: TaskId,
    status: TaskStatus,
    input: String,              // 原始转写文本
    classification: Classification,
    agent_state: AgentState,    // ReAct 循环状态
    created_at: DateTime,
    updated_at: DateTime,
    steps: Vec<AgentStep>,      // Thought/Action/Observation 记录
}
```

#### 4.5.2 并发调度

- **最大并行度**: 3 (可配置)
- **默认策略**: FIFO 队列 + 优先级插队
  - 新任务追加到队列尾部
  - High/Critical 优先级任务插队到队首
  - Critical 优先级中断当前执行中任务
- **上下文判断**: 新输入由 classifier 判断是新任务还是补充当前上下文
  - 补充上下文: 追加文本到当前任务，Reasoner 重新推理
  - 新任务: 加入队列

#### 4.5.3 打断与恢复

**打断**:
- 新输入经 classifier 判断为 "补充当前上下文"
- 当前步骤完成后，注入新上下文继续推理
- Critical 优先级任务会抢占运行中的低优先级任务，低优先级任务转入 Paused

**取消**:
- 用户通过 UI 取消任务
- 终止当前 ReAct 循环
- 已执行的工具操作不可回滚

**暂停/恢复**:
- 用户通过 UI 暂停任务
- 保存完整 AgentState (包括步骤历史)
- 恢复时从暂停点继续

#### 4.5.4 任务生命周期

```rust
impl TaskExecutor {
    fn submit(text: String) -> TaskId;
    fn cancel(task_id: TaskId);
    fn pause(task_id: TaskId);
    fn resume(task_id: TaskId);
    fn list_active() -> Vec<TaskInfo>;
    fn list_history(page: u32) -> Vec<TaskRecord>;
}
```

---

### 4.6 tools

**Crate**: `haven-tools`

#### 4.6.1 工具注册中心

统一注册 MCP 工具、Skills、内置工具:

```rust
struct ToolRegistry {
    mcp_tools: HashMap<String, McpTool>,
    skills: HashMap<String, Skill>,
    builtin_tools: HashMap<String, BuiltinTool>,
}

impl ToolRegistry {
    fn list_all() -> Vec<ToolDescriptor>;
    fn get(name: &str) -> Option<Box<dyn Tool>>;
    fn refresh_mcp();
    fn refresh_skills();
}

trait Tool {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> JsonSchema;
    async fn execute(&self, params: Value) -> Result<ToolOutput>;
    fn requires_confirmation(&self) -> bool;
}
```

#### 4.6.2 MCP 客户端

- 实现 MCP 协议客户端 (stdio / SSE 传输)
- 启动时连接配置的 MCP 服务器
- 自动发现: 扫描用户配置的 MCP 端点
- 手动配置: 设置页面添加 MCP 服务器
- STT 通过 MCP 调用 (用户自配 STT 服务器)
- 断线重连: 指数退避重试，达到上限后标记离线并提示用户

#### 4.6.3 Skills 引擎

**目录结构** (社区约定格式):

```
skills/
├── file-organizer/
│   ├── SKILL.md          # 【必填】元数据 + 执行指令
│   ├── references/       # 参考文档
│   ├── templates/        # 输出模板
│   └── scripts/          # 配套小脚本
├── system-monitor/
│   ├── SKILL.md
│   └── scripts/
│       └── monitor.py    # 首发支持 Python
└── ...
```

**SKILL.md 格式**:

```markdown
# Skill: File Organizer

## Metadata
- name: file-organizer
- description: 按文件类型自动整理目录
- allowed_tools: [file_read, file_move, file_delete]
- version: 1.0.0
- language: python

## Instructions
(自然语言描述，由 LLM 理解并驱动脚本执行)
...
```

**脚本约定**:
- 首发支持 Python
- 脚本通过 `scripts/` 目录存放
- LLM 根据 SKILL.md 的 Instructions 生成或调用脚本
- 插件式架构预留其他语言支持

**管理方式**:
- 目录扫描自动发现: 启动时扫描 `skills/` 目录
- UI 管理页面: 安装/启用/禁用/卸载 Skills
- 热重载: `refresh_skills()` 重新扫描

#### 4.6.4 内置工具 (首发)

| 工具类 | 功能 | 需确认 |
|--------|------|--------|
| **文件操作** | 读文本、写文本、删除文件/目录、移动/复制、列出目录、获取文件信息 | 写/删除/移动 → 是；读/列 → 否 |
| **进程管理** | 列出进程、查询进程详情、终止进程 | 终止 → 是；列出/查询 → 否 |
| **剪贴板** | 读取剪贴板文本、写入剪贴板文本、清空剪贴板 | 写入 → 是；读取 → 否 |

**截图**: 首发不内置，计划后续版本内置。

#### 4.6.5 安全确认网关

所有工具调用需经过安全确认网关:

```rust
struct SafetyGateway {
    whitelist: HashSet<String>,         // 用户配置的白名单工具
    session_trusted: HashSet<String>,   // 当前会话内已确认的工具
}

enum ConfirmationResult {
    AutoApproved,           // 白名单或会话内已信任
    RequiresConfirmation {  // 需要弹窗确认
        tool_name: String,
        params: Value,
        risk_level: RiskLevel,
    },
    Blocked,                // 用户显式禁止、策略阻止或参数不合法
}
```

**确认策略**:
1. 每次高危操作弹窗确认 (默认)
2. 同一会话内首次弹窗后信任该工具
3. 用户可配置高危操作白名单 (白名单内自动放行)

**风险等级**:

```rust
enum RiskLevel {
    Safe,      // 只读操作，无需确认
    Low,       // 低风险写入 (剪贴板)
    Medium,    // 文件修改、进程终止
    High,      // 文件删除、系统配置修改
    Critical,  // 格式化、注册表修改等
}
```

---

### 4.7 memory

**Crate**: `haven-memory`

#### 4.7.1 数据库拆分方案

会话边界: 应用启动后默认恢复上次活跃会话；当用户明确发起新对话、手动结束会话或应用退出并清理上下文时创建新会话。

多文件 SQLite 拆分:

| 数据库文件 | 内容 | 说明 |
|------------|------|------|
| `session.db` | 会话记忆 | 当前/近期会话的消息历史 |
| `history.db` | 任务历史 | 完整任务执行记录，保留固定时长 |
| `preferences.db` | 偏好记忆 | 用户偏好、设置、白名单 |
| `facts.db` | 事实记忆 | 跨会话用户事实 (名字、常用路径等) |
| `sessions.db` | 会话索引 | 会话元数据、状态、时间边界 |

#### 4.7.2 会话记忆

- **滑动窗口**: 最近 N 条消息 (默认 50，可配置)
- **存储**: `session.db`
- **类型**: 用户消息 + 助手消息 (含 Thought/Action/Observation)
- **清理**: 超过窗口大小的旧消息自动归档到 `history.db`

#### 4.7.3 偏好记忆

- **自动学习**: 从用户行为中推断偏好 (常用工具、语言、时区)
- **手动配置**: 用户通过设置页面显式设置
- **存储**: `preferences.db` key-value
- **示例**: `preferred_mcp_server`, `default_priority`, `silence_timeout`

#### 4.7.4 任务历史

- **保留策略**: 保留固定时长 (默认 90 天，可配置)
- **存储**: `history.db`
- **记录内容**: 输入文本、分类结果、完整 ReAct 步骤链、工具调用详情、耗时、状态
- **查询**: 支持按时间、关键词、工具名搜索
- **导出**: 支持 JSON 导出

#### 4.7.5 事实记忆

- **来源**: 用户主动告知 + agent 从对话中推断
- **存储**: `facts.db` (subject-predicate-object 三元组)
- **示例**: `("user", "home_directory", "/Users/alice")`, `("user", "preferred_editor", "vscode")`
- **使用**: 注入 system prompt 中作为背景知识

---

### 4.8 notification

**Crate**: 在 `haven-desktop` 中实现

#### 通知类型

```rust
enum Notification {
    TaskComplete(TaskSummary),
    TaskError { task_id: TaskId, error: String },
    ConfirmationRequired(ConfirmRequest),
    StatusChange { message: String },
}
```

#### 通知渠道

| 渠道 | 适用场景 |
|------|----------|
| Windows 原生 toast | 后台运行时的任务结果通知 |
| UI 内通知栏 | 主窗口打开时的实时通知 |
| 系统托盘气泡 | 快速状态提示 |

---

## 5. UI 设计

### 5.1 整体布局

基于聊天式布局构建初版:

```
┌─────────────────────────────────────┐
│  Haven - 设置 ─ ☰ ─ ╳            │  ← 标题栏
├─────────────────────────────────────┤
│                                     │
│  系统: 有什么可以帮你的？           │
│                                     │
│  ┌──────────────────────────────┐   │  ← 任务卡片 (折叠)
│  │ 用户: 帮我整理桌面           │   │
│  │ 思考中...                   │   │
│  │ 已完成 (3 步, 2.1s)         │   │
│  └──────────────────────────────┘   │
│                                     │
│  用户: 打开记事本                   │
│  助手: 正在执行...                  │
│                                     │
│  ───────────── 消息区域 ──────────  │
│                                     │
│  ┌──────────────────────────────┐   │
│  │ 输入框 (或显示录音状态)     │   │
│  │ 录音中...  00:05           │   │
│  └──────────────────────────────┘   │
│  按 Ctrl+Shift+Space 开始 │ 设置 ─ │  ← 状态栏
└─────────────────────────────────────┘
```

### 5.2 页面导航

Tab 导航:

| Tab | 内容 |
|-----|------|
| 对话 | 聊天式主面板 (任务队列、实时推理) |
| 工具 | MCP 配置、Skills 管理 |
| 历史 | 任务历史搜索、查看、导出 |
| 设置 | 通用设置、LLM 配置、热键绑定 |

### 5.3 关键 UI 组件

#### 5.3.1 任务卡片

每个任务显示为可展开卡片:
- 折叠: 用户输入摘要 + 状态图标 + 耗时
- 展开: 完整 ReAct 步骤 (Thought / Action / Observation) 时间线

#### 5.3.2 确认弹窗

```
┌─────────────────────────────────────┐
│  高危操作确认                       │
│                                     │
│  工具: 删除文件                     │
│  参数: C:\Users\alice\important.txt │
│  风险等级: 高                       │
│                                     │
│  [ ] 本次会话内信任此工具           │
│                                     │
│  [ 确认 ]  [ 取消 ]                │
└─────────────────────────────────────┘
```

#### 5.3.3 录音状态指示

- 录音中: 红色呼吸灯动画 + 时长计时器
- 静音中: 波形占位符
- 转写中: 加载动画 + "转写中..."

#### 5.3.4 实时推理流

ReAct 循环的每一步以流式推送:
- Thought: 低对比度文本，实时流式渲染
- Action: 高亮工具调用卡片
- Observation: 缩进的结构化输出展示

---

## 6. 数据模型

### 6.1 核心表结构

#### sessions.db

```sql
CREATE TABLE sessions (
    id          TEXT PRIMARY KEY,
    started_at  TEXT NOT NULL,
    ended_at    TEXT,
    status      TEXT NOT NULL DEFAULT 'active'  -- 'active' | 'closed'
);
```

#### session.db

```sql
-- 会话记忆 (滑动窗口)
CREATE TABLE messages (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES sessions(id),
    role        TEXT NOT NULL,  -- 'user' | 'assistant'
    content     TEXT NOT NULL,
    message_type TEXT,          -- 'text' | 'thought' | 'action' | 'observation'
    created_at  TEXT NOT NULL   -- ISO 8601
);

CREATE INDEX idx_messages_session ON messages(session_id, created_at);
```

#### history.db

```sql
-- 任务记录
CREATE TABLE tasks (
    id            TEXT PRIMARY KEY,
    input_text    TEXT NOT NULL,
    classification TEXT NOT NULL,  -- JSON: Classification
    status        TEXT NOT NULL,   -- 'completed' | 'cancelled' | 'error'
    created_at    TEXT NOT NULL,
    completed_at  TEXT,
    duration_ms   INTEGER,
    total_steps   INTEGER
);

-- ReAct 步骤记录
CREATE TABLE steps (
    id            TEXT PRIMARY KEY,
    task_id       TEXT NOT NULL REFERENCES tasks(id),
    step_number   INTEGER NOT NULL,
    step_type     TEXT NOT NULL,  -- 'thought' | 'action' | 'observation'
    content       TEXT NOT NULL,
    tool_name     TEXT,           -- 仅 action
    tool_params   TEXT,           -- JSON, 仅 action
    tool_output   TEXT,           -- JSON, 仅 observation
    duration_ms   INTEGER,
    created_at    TEXT NOT NULL
);

CREATE INDEX idx_steps_task ON steps(task_id, step_number);
CREATE INDEX idx_tasks_created ON tasks(created_at);
```

#### preferences.db

```sql
CREATE TABLE preferences (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE whitelist (
    tool_name   TEXT NOT NULL PRIMARY KEY,
    pattern     TEXT,         -- 可选的参数匹配模式
    added_at    TEXT NOT NULL
);

CREATE TABLE mcp_servers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    transport   TEXT NOT NULL, -- 'stdio' | 'sse'
    config      TEXT NOT NULL, -- JSON
    enabled     INTEGER NOT NULL DEFAULT 1
);
```

#### facts.db

```sql
CREATE TABLE facts (
    id          TEXT PRIMARY KEY,
    subject     TEXT NOT NULL,
    predicate   TEXT NOT NULL,
    object      TEXT NOT NULL,
    source      TEXT NOT NULL,   -- 'user' | 'inferred'
    confidence  REAL DEFAULT 1.0,
    created_at  TEXT NOT NULL
);

CREATE INDEX idx_facts_subject ON facts(subject);
```

### 6.2 配置模型

```rust
struct AppConfig {
    audio: AudioConfig,
    llm: LlmConfig,
    hotkey: HotkeyConfig,
    task: TaskConfig,
    memory: MemoryConfig,
    notification: NotificationConfig,
    security: SecurityConfig,
}

struct HotkeyConfig {
    mode: HotkeyMode,             // Toggle | Hold
    key_binding: String,          // "Ctrl+Shift+Space"
    mute_hotkey: Option<String>,  // 可选静音快捷键
}

struct TaskConfig {
    max_concurrent: usize,        // 3
    max_steps: u32,               // 30
    default_priority: TaskPriority,
}

struct MemoryConfig {
    session_window_size: usize,   // 50
    history_retention_days: u32,  // 90
}

struct SecurityConfig {
    confirmation_mode: ConfirmationMode,  // Always | SessionTrust | Whitelist
    whitelist: Vec<String>,
    encrypt_sensitive: bool,      // true
}
```

---

## 7. 安全设计

### 7.1 分层安全策略

```
┌─────────────────────────────────────────┐
│  用户交互层                              │
│  ├─ 高危操作弹窗确认                      │
│  ├─ MCP 工具默认需确认                    │
│  └─ 白名单管理                            │
├─────────────────────────────────────────┤
│  安全网关层                              │
│  ├─ 风险等级评估                          │
│  ├─ 白名单匹配                            │
│  └─ 会话信任管理                          │
├─────────────────────────────────────────┤
│  工具执行层                              │
│  ├─ 参数校验与沙盒化                      │
│  ├─ 路径边界检查                          │
│  └─ 进程权限控制                          │
├─────────────────────────────────────────┤
│  数据保护层                              │
│  ├─ 敏感配置 AES-256-GCM 加密 (keyring)  │
│  ├─ 音频仅本地处理                        │
│  └─ 云端仅传输转写文本                    │
└─────────────────────────────────────────┘
```

### 7.2 具体措施

| 措施 | 实现 |
|------|------|
| API Key 加密 | Windows Credential Manager / keyring-rs，内存中不缓存明文 |
| 音频隐私 | CPAL 捕获后仅在进程内传递 WAV，通过 MCP 发送给本地 STT 服务器 |
| 工具审计 | 每次工具调用记录完整输入/输出/耗时到 `history.db` |
| 路径约束 | 内置工具限制在用户目录内操作，系统目录默认只读或需确认 |
| MCP 隔离 | MCP 进程以子进程运行，崩溃不影响主进程 |

---

## 8. 开发阶段与 Crate 拆分

### 8.1 里程碑划分

#### M1: 桌面壳 + UI 骨架

| 模块 | crate | 交付物 |
|------|-------|--------|
| 项目初始化 | workspace | Cargo workspace + Tauri 项目骨架 |
| 系统托盘 | `haven-desktop` | 托盘图标 + 右键菜单 (显示/静音/设置/退出) |
| 热键注册 | `haven-desktop` | `Ctrl+Shift+Space` 全局热键 |
| 开机自启 | `haven-desktop` | Task Scheduler 注册 + 设置开关 |
| 基础 UI | SvelteKit | 聊天式布局骨架 + 4 页面导航 |
| Tauri 桥 | `haven-bridge` | invoke/emit 基础框架 |
| 配置模型 | `haven-common` | AppConfig + Settings 持久化 |
| LLM 抽象 | `haven-llm` | classifier/reasoner/fallback 端点模型 |
| 记忆骨架 | `haven-memory` | sessions/session/history/preferences/facts 数据层 |

#### M2: 录音 + 转写闭环

| 模块 | crate | 交付物 |
|------|-------|--------|
| 音频捕获 | `haven-input` | CPAL 录音 + WAV 输出 |
| VAD | `haven-input` | Silero VAD ONNX 推理 |
| 录音控制 | `haven-desktop` | PTT/Toggle 双模式 |
| MCP 客户端 | `haven-tools` | MCP stdio 客户端基础实现 |
| STT 集成 | `haven-input` | MCP STT 调用 + 转写结果接收 |
| 录音 UI | SvelteKit | 录音状态指示 + 转写结果展示 |

#### M3: Agent 核心

| 模块 | crate | 交付物 |
|------|-------|--------|
| LLM 客户端 | `haven-llm` | 多模型端点抽象 |
| LLM 路由 | `haven-agent` | classifier/reasoner/fallback 路由 |
| ReAct 循环 | `haven-agent` | Thought→Action→Observation 状态机 |
| 意图分类 | `haven-agent` | 新任务 / 补充上下文分类 |
| 任务队列 | `haven-task` | 任务创建/取消/暂停/恢复 |
| 并发调度 | `haven-task` | 3 并行 + FIFO + 优先级插队 |
| 内置工具 | `haven-tools` | 文件操作 + 进程管理 + 剪贴板 |
| 安全网关 | `haven-tools` | 风险评级 + 确认流程 |
| Agent UI | SvelteKit | 实时推理流展示 + 任务卡片 |

#### M4: 工具生态

| 模块 | crate | 交付物 |
|------|-------|--------|
| Skills 引擎 | `haven-tools` | 目录扫描 + SKILL.md 解析 + Python 脚本执行 |
| MCP 自动发现 | `haven-tools` | 端点扫描 + 工具列表获取 |
| MCP UI 配置 | SvelteKit | MCP 服务器添加/启用/禁用 |
| Skills UI | SvelteKit | Skills 安装/管理页面 |
| 确认弹窗 | SvelteKit + `haven-bridge` | 高危操作确认对话框 |

#### M5: 并发与打断

| 模块 | crate | 交付物 |
|------|-------|--------|
| 上下文补充 | `haven-agent` | 追加文本到当前任务 |
| 智能打断 | `haven-agent` | 新输入实时判断 |
| 优先级调度 | `haven-task` | Critical 中断 + High 插队 |
| 通知系统 | `haven-desktop` | Windows toast + UI 通知栏 |

#### M6: 记忆与稳定性

| 模块 | crate | 交付物 |
|------|-------|--------|
| 会话记忆 | `haven-memory` | 滑动窗口消息持久化 |
| 偏好记忆 | `haven-memory` | 自动学习 + 手动配置 |
| 任务历史 | `haven-memory` | 全量记录 + 搜索/导出 |
| 事实记忆 | `haven-memory` | 三元组存储 + 推断 |
| 日志系统 | `haven-common` | 可配置日志级别 + 文件输出 |
| 错误恢复 | 全局 | 崩溃恢复、断网重连 |
| 性能优化 | 全局 | 索引优化、内存管理 |

### 8.2 Crate 依赖关系

```
     haven-desktop --> haven-bridge --> haven-agent --> haven-llm
          |                                  |
          |                                  |--> haven-task
          |                                  |
          |--> haven-input                   |--> haven-tools
          |                                  |
          |--> haven-common <----------------|
                                              |
                                   haven-memory --> haven-common
```

`haven-common` 被所有 crate 依赖，定义共享类型。

---

## 9. 附录

### 9.1 待确定项

以下项目在本次设计中未完全明确，需后续迭代补充:

| 项目 | 状态 | 说明 |
|------|------|------|
| 滑动窗口大小 | 默认 50，待验证 | 需根据 token 消耗调整 |
| 任务历史保留天数 | 默认 90，待确认 | 需评估存储空间 |
| 热键自定义 UI | 已规划 | 设置页面中的热键绑定组件 |
| 跨平台架构细节 | 架构预留 | macOS/Linux 适配计划 |
| 本地 LLM 方案 | 备选 | 如 Ollama/llama.cpp 集成 |
| Skills 沙盒执行 | M4 深化 | Python 脚本的安全隔离 |
| STT MCP 工具名称约定 | 需标准化 | 确保 STT MCP 互操作性 |
| 截图工具后续版本 | M4+ | 屏幕捕获 + 选区交互 |

### 9.2 术语表

| 术语 | 说明 |
|------|------|
| ReAct | Reasoning + Acting 循环，Thought → Action → Observation |
| PTT | Push-to-Talk，按住说话模式 |
| VAD | Voice Activity Detection，语音活动检测 |
| MCP | Model Context Protocol，工具调用协议 |
| Skills | 本地脚本模板，通过自然语言驱动 Python 脚本执行 |
| Classifier | 意图分类模型，判断任务类型和工具路由 |
| Reasoner | 主推理模型，执行 ReAct 循环 |
| Fallback | 降级备用模型，Reasoner 不可用时激活 |