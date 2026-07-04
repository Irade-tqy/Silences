# Product

## Register

product

## Users

**独立开发者** — 熟悉命令行和 AI 工具链的技术用户，在本地环境中运行 Agentic Coding。他们关注：
- 开源透明（代码可审计、自部署）
- 数据安全（所有请求本地处理，不依赖第三方云）
- 低成本（按需付费 DeepSeek API，无隐藏订阅）
- 可控性（能调整模型参数、工具行为、系统提示）

## Product Purpose

Silences 是一个开源的 Agentic Coding 框架，让开发者在本地终端 + Web UI 中通过自然语言与 AI Agent 协作编写代码。它连接 DeepSeek 等大语言模型，提供文件操作、命令行执行、代码读写等工具化编码能力。

核心价值：**用最少的成本、最可控的方式，在日常编码中获得 AI Agent 的全部能力。**

## Brand Personality

简洁、可靠、稳定

- **简洁** — 界面无冗余装饰，信息层级清晰，操作路径最短
- **可靠** — 每一个操作都有明确的反馈，错误状态不隐藏，Token 用量透明可见
- **稳定** — 视觉一致性高，交互可预期，不会出现闪烁/跳动/意外布局偏移

## Anti-references

- 不要像传统 IDE 那样沉重复杂（大量工具栏/面板/配置向导）
- 不要像 SaaS 产品那样用花哨的营销感 UI（大渐变、玻璃拟态、夸张动画）
- 不要像 Cursor/Windsurf 那样"编辑器优先"——Silences 是对话优先的 Agent 工具，不是 IDE

## Design Principles

1. **对话即界面** — 聊天面板是核心交互区域，所有工具调用、代码编辑都在对话流中完成，不分离出独立编辑器
2. **透明胜于魔法** — Token 消耗、推理过程、工具调用结果都对用户可见，不隐藏 LLM 的工作过程
3. **工具应消失在任务中** — UI 不抢戏，配色、间距、动效都以支持编码任务为目标，不为装饰而装饰
4. **暗色为默认** — 开发者工具在夜间/弱光环境下使用频率高，暗色模式是默认体验而非"可选主题"
5. **一致性是可靠的外在表现** — 组件库复用同一套 token，所有交互组件都覆盖 default/hover/focus/active/disabled/loading/error 状态

## Accessibility & Inclusion

- WCAG 2.1 AA 标准，重点保证暗色背景上的文本对比度
- 支持 prefers-reduced-motion，动效仅用于状态反馈
- 支持键盘导航（Tab/Enter/Esc 操作所有交互元素）
