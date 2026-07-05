# Silences — Agentic Coding 框架

> 版本草案 · 2026-07-01  
> 定位：自用开源，Claude Code 替代品  
> 核心理念：极简 · 可控 · 成本透明 · 前后端彻底分离

---

## 1. 为什么做

### 问题
- **Claude Code** 好用但不可控。Anthropic 的立场和闭源决策意味着你无法真正信任这个工具
- **CodeWhale** 同任务消耗 3x+ 成本，不知道为什么就是比 CC 贵
- **Reasonix** Windows 上不可用（滚轮没适配、界面混乱）
- 市面上的 agentic coding 工具要么太贵，要么太复杂，要么不透明

### 目标
做一个**自己可控的、成本 ≤ CC 的、极简的** agentic coding 框架。不是为了替代 CC 的市场，而是为了有一把自己完全信任的刀。

---

## 2. 设计哲学

### 2.1 前后端完全、彻底分离

```
┌──────────┐     MCP/HTTP      ┌──────────┐     MCP/HTTP      ┌──────────┐
│  前端     │ ←──────────────→  │  后端     │ ←──────────────→  │  Plugin  │
│ (仅渲染)  │     SSE 流式      │ (核心逻辑)  │                  │ (MCP)    │
└──────────┘                   └──────────┘                   └──────────┘
                                      │
                                      ▼
                               ┌──────────┐
                               │  模型 API  │
                               │ (DeepSeek │
                               │  / 任意)  │
                               └──────────┘
```

- **后端**可以独立运行，不依赖前端。可通过命令行、脚本或其他软件调用
- **前端**仅负责渲染，不存 localStorage（如果是网页方案），不处理任何核心逻辑
- 所有核心逻辑（对话管理、工具调度、上下文、成本计算）均在后端

### 2.2 极简

开箱仅提供：
- 基础对话（流式输出）
- 工具调用（tool call）
- Plugin 系统（适配 MCP 协议）
- Skills 机制（可加载的技能模板）

**明确不做**（v1 范围外）：
- ❌ 复杂模型配置、多密钥管理
- ❌ Online / Gateway（水太深）
- ❌ 跨会话记忆
- ❌ 复杂的 Web 前端
- ❌ 复杂 hook 系统
- ❌ 内置 Web Search tool
- ❌ 用户系统 / 权限

### 2.3 可控

- **流式输出**：必须，从后端到前端全链路 SSE / WebSocket
- **可观察**：用户可以随时查看当前状态、工具调用详情、上下文内容
- **可停止**：用户可以随时中断当前操作
- **显示模式**：只有两极
  - **极简**：纯终端日志流，tail -f 风格，唯一 UI 是输入框 + 停止按钮
  - **极繁**：结构化面板，左侧文件树/会话列表，中对话流，右 tool call / token / 上下文详情
  - 不存在中间态，设置切换

### 2.4 成本控制 > 一切

成本是唯一核心指标。一切设计围绕降低 API 花费：

- **上下文管理极致**：前缀缓存命中率优先，尽量减少每次请求的冗余 token
- **Tool 结果精细控制**：每轮 tool call 可以分别控制哪些结果在前端显示、哪些不显示（但都计入上下文）
- **Cost 仪表盘**：每次会话结束自动输出 cost breakdown（prompt 缓存命中率、tool call 花费、各轮次花费）
- **上限保护默认全开**：max_tokens、轮次、cost 三项硬上限，用户可调但默认开启

### 2.5 Harness 友好

- 支持**项目级**和**用户级**提示词自由配置
- 会话文件是 `.jsonl` / `.md` 格式，可 git 追踪、可 diff、可 replay
- 日志按模块分文件、时间戳命名，持久化存储，方便 debug

### 2.6 平台

- **Windows 一等人**（vs Reasonix 的盲区）
- 但架构保证跨平台：后端纯 Rust，所有平台相关代码隔离在 adapter 层

---

## 3. 架构

### 3.1 后端

```
Language: Rust
Runtime: tokio
HTTP: axum
```

**模块**（2026-07-02 当前状态）：

```
crates/
  silences-core/       # 核心类型：Message, ToolCallValue, TokenUsage, SseEvent
  silences-llm/        # DeepSeek API 调用（流式/非流式 + tool_calls 解析）
  silences-db/         # SQLite 持久化（会话/消息/用量）
  silences-agent/      # Agent 循环 + 9 个内建工具
                         agent.rs       — LLM↔tool 循环
                         toolcall/      — 工具调度中心
                           mod.rs       — ToolDef 注册 + 路由
                           glance.rs    — 目录/文件概览
                           grep.rs      — 正则搜索
                           read.rs      — 读文件（行号范围）
                           create.rs    — 新建文件
                           edit.rs      — 精准替换（按行号最近匹配）
                           replace.rs   — 批量替换
                           regret.rs    — 撤销（5 条历史，含逆操作引擎）
                           command.rs   — PowerShell 执行
                           trash.rs     — 安全删除（.trash 回收站）
  silences-server/     # HTTP/SSE 服务端（整合 agent 循环）
  silences-cli/        # TUI/CLI 前端（raw mode 输入）
```

### 3.2 前端

```
Primary:   TUI (ratatui)          # 首屏、极简模式
Secondary: Web (React/Vue)        # 极繁模式、参考实现
Tertiary:  CLI (raw stdin/stdout)  # 脚本调用、管道
```

前端**不包含任何业务逻辑**——它只是后端 API 的一个渲染客户端。

### 3.3 Plugin 系统

接口：MCP 协议（Model Context Protocol）、Skills
- 不加私有 plugin 格式
- MCP server 跑在子进程中，崩溃不影响主进程
- Plugin 有资源限额（内存、调用频率、总耗时）

### 3.4 会话与记录

```
sessions/
  2026-07/
    01T12-00-00.jsonl    # 完整会话：每条消息 + tool call + cost
    01T14-30-00.jsonl    # 可 git 追踪、可 replay
```

会话文件是可复现的。`--replay` 模式重放会话但不调用 API，只看 cost 和上下文变化。

---

## 4. 成本模型

### 4.1 核心指标

**Effective Cost per Task** = 总 API 花费 / 任务完成率

### 4.2 辅助指标

- 峰值上下文长度
- Prompt 缓存命中率
- 框架内部处理 overhead（不经 API 的 token 运算）
- 首 token 延迟

### 4.3 成本仪表盘输出示例

```
────────────────────────────────────────
  会话摘要
────────────────────────────────────────
  总花费:       $0.0128
  总轮次:       3
  总耗时:       4分05秒

  Token:
    输入:         45,000 (缓存命中: 32,000 = 71%)
    输出:          6,200

  每轮明细:
    1. $0.0055  (输入 18k, 输出 2k, 缓存 62%)
    2. $0.0048  (输入 15k, 输出 2.2k, 缓存 75%)
    3. $0.0025  (输入 12k, 输出 2k, 缓存 83%)

  最贵 tool call:
    edit_file   $0.0012  (输出 1.8k tokens)
    bash        $0.0009  (输出 12k tokens)

  缓存趋势:  62% → 75% → 83%  ↑
────────────────────────────────────────
```

---

## 5. 开发路线

### v0.1 ✅ — 骨架（已完成）

- [x] Rust 项目结构搭建
- [x] `silences-core`: 基础类型定义
- [x] `silences-llm`: DeepSeek provider（流式调用）
- [x] GUI 前端：极简对话
- [x] 基本的 prompt 配置（项目级+用户级）
- [x] 会话管理 + SQLite 持久化
- [x] Token 用量追踪 + 成本计算

### v0.2 ✅ — 内建工具系统（已完成）

- [x] `silences-agent`: Tool call 循环（LLM → tool → LLM）
- [x] 9 个内建工具：glance / grep / read / create / edit / replace / regret / command / trash
- [x] 工具描述遵循 what + why + how 三段式
- [x] DeepSeek 原生 `tool_calls` API（streaming）
- [x] regret 逆操作引擎（5 条历史，支持 edit/create/trash/replace）
- [x] reasoning + tool_calls 共存识别
- [x] 纯文本 = 完成，reasoning 不算
- [x] edit 按行号最近匹配（line 必填）
- [x] 所有路径使用绝对目录
- [x] SSE 流式转发（text/reasoning/tool call 摘要）
- [x] 消息和用量自动持久化到 DB

### v0.3 — 上下文与成本（待开始）

- [x] 任务列表机制+上下文回滚
- [x] Token 计数优化、成本跟踪完善
- [x] Cost 仪表盘
- [ ] 侧边栏手术刀管理上下文
- [ ] 模型自驱动管理（fork 进行尝试）

### v0.4 — MCP 插件（待开始）

- [ ] `silences-mcp`: MCP client
- [ ] MCP server 进程管理
- [ ] 内建工具逐步迁移到 MCP

### v0.5 — Benchmark v1（待开始）

- [ ] 注入 2 个 bug 到 dailyPlanner
- [ ] 写 Judge 后端
- [ ] 跑 CC baseline
- [ ] 跑 CodeWhale baseline
- [ ] 跑 Silences baseline → 对比

---

## 6. 参考目标

| 维度 | 目标 | 测量方式 |
|------|------|---------|
| Debug 场景 cost | ≤ CC | Benchmark v0.4 |
| Feature 场景 cost | ≤ CC | Benchmark v0.6 |
| 框架 overhead | ≤ 5% API 时间 | 对比纯 curl 调用 |
| Windows 运行 | 原生体验 | CI 跑 Windows runner |
| 缓存命中率 | ≥ 90% 稳态 | 会话摘要输出 |
