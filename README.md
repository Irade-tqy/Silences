# Silences — 手术刀般精准的 AI 编码助手

Silences 是一个基于 DeepSeek 的 agentic 编码助手框架。它不是一个 chat 窗口，而是一个**可编程的 agent 运行时**——精确控制工具调用、上下文窗口、对话历史，为软件工程任务量身定制。

## 架构

```
┌──────────────────┐  ┌──────────────────┐
│  silences-server │  │   silences-lib   │
│  (HTTP/SSE 服务)  │  │  (库模式接口)    │
├──────────────────┤  ├──────────────────┤
│  silences-agent  │  │  silences-agent  │
│  (agent 循环)    │  │  (agent 循环)    │
└───────┬──────────┘  └───────┬──────────┘
        │                      │
┌───────┴──────────────────────┴──────────┐
│  silences-llm    silences-db            │
│  (LLM 客户端)    (SQLite 持久化)       │
├──────────────────────────────────────────┤
│              silences-core               │
│        (核心类型: 消息, 工具, 配置)       │
└──────────────────────────────────────────┘
```

### Crate 职责

| Crate | 用途 |
|-------|------|
| **silences-core** | 核心类型定义：Message、ToolCall、Config、Session |
| **silences-llm** | LLM API 客户端：DeepSeek Chat、thinking/reasoning、流式响应 |
| **silences-db** | SQLite 持久化：消息、配置、session、settings |
| **silences-agent** | Agent 主循环：工具调度、上下文管理、任务队列 |
| **silences-server** | HTTP/SSE 服务：实时流推送、REST API、会话管理 |
| **silences-lib** | 库模式接口：供外部 Rust crate 以阻塞方式调用 Agent |

`silences-server` 和 `silences-lib` 是并行的两种运行模式——一个提供 HTTP 服务，另一个供程序化嵌入。

## 特性

- **手术刀式工具调用** — 9 种精准工具：read、edit、block_edit、grep、glob、run、任务系统、上下文管理
- **Thinking/Reasoning 感知** — 原生支持 DeepSeek reasoning_content，完整记录 thinking 过程
- **上下文压缩** — Python 侧边栏脚本实时压缩上下文，控制 token 消耗
- **SSE 实时流** — 服务端推送 agent 思考过程、工具调用、增量输出
- **SQLite 全文持久化** — 所有消息、配置、session 状态持久化，支持历史回溯
- **Benchmark 框架** — AgentBench Scenario A/B 基准测试，验证 agent 实际修复代码的能力

## 快速开始

### 前置依赖

- Rust 1.80+
- Node.js 20+
- DeepSeek API key（环境变量 `DEEPSEEK_API_KEY` 或 `silences.db` 中配置）

### 后端

```bash
# 运行 HTTP 服务
cargo run -p silences-server

# 或直接使用库模式（程序化调用）
cargo test --test your_integration_test -- --nocapture
```

### 前端

```bash
cd web
npm install
npm run dev
# → http://localhost:3000
```

## 配置

通过 `silences.db`（SQLite）或环境变量配置：

- `DEEPSEEK_API_KEY` — LLM API 密钥
- `SILENCES_DB_PATH` — 数据库路径（默认 `./silences.db`）
- `BENCH_WORKTREE` — 基准测试 worktree 路径

## 基准测试

```bash
# Scenario A：修复 worktree 中的 2 个 Pomodoro timer bug
cargo test -p silences-lib --test benchmark_scenario_a -- --nocapture --ignored
```

## 许可

Apache 2.0
