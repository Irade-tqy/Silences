export interface Session {
  id: string;
  created_at: string;
  preview?: string;
  name?: string;
}

export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
  cost_yuan: number;
}

export interface ToolCallEntry {
  id?: string;
  name: string;
  args: string;
  result?: string;
}

export interface Message {
  role: 'user' | 'assistant' | 'tool';
  content: string;
  reasoning?: string;
  isStreaming?: boolean;
  toolCalls?: ToolCallEntry[];
}

export interface AppSettings {
  api_key: string | null;
  system_prompt: string | null;
  warmup_enabled: boolean;
}

export interface RawToolCall {
  id: string;
  type: string;
  function: { name: string; arguments: string };
}

/** 后端 messages_to_view 处理后的工具调用格式（嵌入 result，无 id/type/function） */
export interface ViewToolCall {
  name: string;
  args: string;
  result?: string;
}

/** 后端 messages_to_view 处理后返回的消息格式 */
export interface ViewMessage {
  role: string;
  content: string;
  reasoning_content?: string;
  tool_calls?: ViewToolCall[];
}

export interface RawMessage {
  role: string;
  content: string;
  name?: string;
  reasoning_content?: string;
  tool_calls?: RawToolCall[];
  tool_call_id?: string;
}

export interface Task {
  id: string;
  description: string;
}

export interface SessionState {
  context: RawMessage[];
  tasks: Task[];
  status: string;
}
