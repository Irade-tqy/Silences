'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import Markdown from '@/components/Markdown';

// ─── Types ───────────────────────────────────────────────

interface Session {
  id: string;
  created_at: string;
  preview?: string;
}

interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
  cache_hit_tokens: number;
  cache_miss_tokens: number;
  cost_yuan: number;
}

interface Message {
  role: 'user' | 'assistant' | 'tool';
  content: string;
  reasoning?: string;
  isStreaming?: boolean;
  toolCalls?: ToolCallEntry[];
}

interface ToolCallEntry {
  name: string;
  args: string;
  result?: string;
}

interface AppSettings {
  api_key: string | null;
  system_prompt: string | null;
}

// ─── Config ──────────────────────────────────────────────

const API = process.env.NEXT_PUBLIC_SILENCES_API || 'http://127.0.0.1:0412';

function fmtTime(iso: string) {
  const d = new Date(iso);
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fmtRelative(iso: string): string {
  const now = Date.now();
  const diff = now - new Date(iso).getTime();
  const sec = Math.floor(diff / 1000);
  const min = Math.floor(sec / 60);
  const hr = Math.floor(min / 60);
  const day = Math.floor(hr / 24);
  const wk = Math.floor(day / 7);
  const mo = Math.floor(day / 30);
  const yr = Math.floor(day / 365);
  if (sec < 60) return '刚刚';
  if (min < 60) return `${min} 分钟前`;
  if (hr < 24) return `${hr} 小时前`;
  if (day < 7) return `${day} 天前`;
  if (wk < 5) return `${wk} 周前`;
  if (mo < 12) return `${mo} 月前`;
  return `${yr} 年前`;
}

function fmtCost(yuan: number) {
  if (yuan < 0.0001) return '¥0';
  return `¥${yuan.toFixed(3)}`;
}

function copyText(text: string) {
  navigator.clipboard.writeText(text).catch(() => {});
}

function fmtNum(n: number): string {
  if (n >= 1_000_000_000_000) return (n / 1_000_000_000_000).toFixed(1) + 't';
  if (n >= 1_000_000_000) return (n / 1_000_000_000).toFixed(1) + 'b';
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'm';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k';
  return n.toString();
}

// ─── Page ────────────────────────────────────────────────

export default function Page() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [loading, setLoading] = useState(false);
  const [totalUsage, setTotalUsage] = useState<TokenUsage | null>(null);
  const [roundUsage, setRoundUsage] = useState<TokenUsage | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [collapsedThinking, setCollapsedThinking] = useState<Record<number, boolean>>({});
  const [collapsedToolCalls, setCollapsedToolCalls] = useState<Record<string, boolean>>({});
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  // settings = 服务端返回的已保存值（掩盖后的 api_key），用于 placeholder
  // settingsDirty = 输入框当前编辑值
  const [settings, setSettings] = useState<AppSettings>({ api_key: null, system_prompt: null });
  const [settingsDirty, setSettingsDirty] = useState<AppSettings>({ api_key: '', system_prompt: '' });
  const [settingsSaving, setSettingsSaving] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const msgEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const thinkingIdxRef = useRef<number | null>(null);

  const scrollToBottom = useCallback(() => {
    msgEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, []);
  useEffect(() => { scrollToBottom(); }, [messages, scrollToBottom]);

  const loadSessions = useCallback(async () => {
    try {
      const res = await fetch(`${API}/sessions`);
      if (res.ok) setSessions(await res.json());
    } catch { /* ignore */ }
  }, []);

  useEffect(() => { loadSessions(); }, [loadSessions]);

  const settingsDirtyRef = useRef<AppSettings>({ api_key: '', system_prompt: '' });
  const settingsLoadedRef = useRef(false);
  useEffect(() => { settingsDirtyRef.current = settingsDirty; }, [settingsDirty]);

  const loadSettings = useCallback(async () => {
    try {
      const res = await fetch(`${API}/settings?t=${Date.now()}`);
      if (res.ok) {
        const data: AppSettings = await res.json();
        console.log('GET /settings 响应:', data);
        setSettings(data);
        setSettingsDirty({ api_key: '', system_prompt: data.system_prompt || '' });
      } else {
        console.warn('GET /settings 失败:', res.status);
      }
      settingsLoadedRef.current = true;
      setSettingsLoaded(true);
    } catch (e) {
      console.warn('加载设置失败:', e);
    }
  }, []);

  useEffect(() => { loadSettings(); }, [loadSettings]);

  // 每次打开设置弹窗时重新加载
  useEffect(() => {
    if (settingsOpen) loadSettings();
  }, [settingsOpen, loadSettings]);

  const newSession = useCallback(() => {
    setActiveId(null);
    setMessages([]);
    setTotalUsage(null);
    setRoundUsage(null);
    abortRef.current?.abort();
  }, []);

  const saveSettings = useCallback(async () => {
    setSettingsSaving(true);
    try {
      const cur = settingsDirtyRef.current;
      const body: Record<string, string | null> = {};
      if (cur.api_key && cur.api_key.length > 0) {
        body.api_key = cur.api_key;
      }
      body.system_prompt = cur.system_prompt || null;

      const res = await fetch(`${API}/settings`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (res.ok) {
        const data: AppSettings = await res.json();
        console.log('PUT /settings 响应:', data);
        setSettings(data);
        setSettingsDirty({ api_key: '', system_prompt: data.system_prompt || '' });
      } else {
        console.warn('PUT /settings 失败:', res.status, await res.text().catch(() => ''));
      }
    } catch (e) {
      console.warn('保存设置失败:', e);
    }
    setSettingsSaving(false);
  }, []); // 通过 ref 读取最新值，无需依赖 settingsDirty

  const selectSession = useCallback(async (id: string) => {
    if (loading) return;
    abortRef.current?.abort();
    setActiveId(id);
    setRoundUsage(null);
    setTotalUsage(null);
    setMessages([]);

    try {
      const [msgRes, usageRes] = await Promise.all([
        fetch(`${API}/sessions/${id}/messages`),
        fetch(`${API}/sessions/${id}/usage`),
      ]);
      if (msgRes.ok) {
        const data: { role: string; content: string; reasoning_content?: string }[] = await msgRes.json();
        const msgs: Message[] = data
          .filter(m => m.role === 'user' || m.role === 'assistant')
          .map(m => ({
            role: m.role as 'user' | 'assistant',
            content: m.content,
            reasoning: m.reasoning_content || undefined,
          }));
        setMessages(msgs);
      }
      if (usageRes.ok) {
        const usage: TokenUsage | null = await usageRes.json();
        if (usage) setTotalUsage(usage);
      }
    } catch { /* ignore */ }
  }, [loading]);

  const sendMessage = useCallback(async () => {
    const text = input.trim();
    if (!text || loading) return;
    setInput('');
    setRoundUsage(null);

    const userMsg: Message = { role: 'user', content: text };
    const placeholder: Message = { role: 'assistant', content: '', isStreaming: true };
    setMessages(prev => {
      thinkingIdxRef.current = prev.length + 1; // assistant msg index
      return [...prev, userMsg, placeholder];
    });

    setLoading(true);
    const controller = new AbortController();
    abortRef.current = controller;

    try {
      const res = await fetch(`${API}/chat`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          session_id: activeId || null,
          message: text,
          stream: true,
        }),
        signal: controller.signal,
      });

      if (!res.ok) {
        const errText = await res.text().catch(() => '');
        setMessages(prev => prev.map((m, i) =>
          i === prev.length - 1 && m.isStreaming
            ? { role: 'assistant', content: `错误: ${res.status} ${errText}`, isStreaming: false }
            : m
        ));
        setLoading(false);
        return;
      }

      const reader = res.body?.getReader();
      if (!reader) throw new Error('No reader');

      let buffer = '';
      let newSid = activeId || '';
      let reasoningBuf = '';
      const decoder = new TextDecoder();

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const raw of lines) {
          const line = raw.trim();
          if (!line || line.startsWith('event:') || line.startsWith('id:')) continue;
          const jsonStr = line.startsWith('data: ') ? line.slice(6) : line;
          if (!jsonStr) continue;

          try {
            const ev = JSON.parse(jsonStr);

            if (ev.type === 'session') {
              newSid = ev.id;
              if (!activeId) setActiveId(ev.id);
            } else if (ev.type === 'text') {
              setMessages(prev => prev.map((m, i) =>
                i === prev.length - 1 && m.isStreaming
                  ? { ...m, content: m.content + ev.content }
                  : m
              ));
            } else if (ev.type === 'reasoning') {
              reasoningBuf += ev.content;
              setMessages(prev => prev.map((m, i) =>
                i === prev.length - 1 && m.isStreaming
                  ? { ...m, reasoning: (m.reasoning || '') + ev.content }
                  : m
              ));
            } else if (ev.type === 'tool_calling') {
              setMessages(prev => prev.map((m, i) =>
                i === prev.length - 1 && m.isStreaming
                  ? { ...m, toolCalls: [...(m.toolCalls || []), { name: ev.name, args: ev.args }] }
                  : m
              ));
            } else if (ev.type === 'tool_result') {
              setMessages(prev => prev.map((m, i) => {
                if (i !== prev.length - 1 || !m.isStreaming) return m;
                const calls = m.toolCalls ? [...m.toolCalls] : [];
                if (calls.length > 0) calls[calls.length - 1] = { ...calls[calls.length - 1], result: ev.summary };
                return { ...m, toolCalls: calls };
              }));
            } else if (ev.type === 'usage') {
              const u = ev as unknown as TokenUsage;
              setRoundUsage(u);
              setTotalUsage(prev => prev ? {
                input_tokens: prev.input_tokens + u.input_tokens,
                output_tokens: prev.output_tokens + u.output_tokens,
                cache_hit_tokens: prev.cache_hit_tokens + u.cache_hit_tokens,
                cache_miss_tokens: prev.cache_miss_tokens + u.cache_miss_tokens,
                cost_yuan: prev.cost_yuan + u.cost_yuan,
              } : u);
            } else if (ev.type === 'error') {
              setMessages(prev => prev.map((m, i) =>
                i === prev.length - 1 && m.isStreaming
                  ? { ...m, content: m.content + `\n⚠️ ${ev.message}`, isStreaming: false }
                  : m
              ));
            }
          } catch { /* skip */ }
        }
      }

      setMessages(prev => prev.map((m, i) =>
        i === prev.length - 1 && m.isStreaming
          ? { ...m, isStreaming: false, reasoning: reasoningBuf || undefined }
          : m
      ));
      // 思考结束自动折叠
      const ti = thinkingIdxRef.current;
      if (ti !== null) {
        setCollapsedThinking(prev => ({ ...prev, [ti]: true }));
        thinkingIdxRef.current = null;
      }

      loadSessions();
      if (!activeId && newSid) setActiveId(newSid);

    } catch (err: unknown) {
      if (err instanceof DOMException && err.name === 'AbortError') return;
      setMessages(prev => prev.map((m, i) =>
        i === prev.length - 1 && m.isStreaming
          ? { role: 'assistant', content: `请求失败: ${err instanceof Error ? err.message : String(err)}`, isStreaming: false }
          : m
      ));
    } finally {
      setLoading(false);
      abortRef.current = null;
    }
  }, [input, loading, activeId, loadSessions]);

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }, [sendMessage]);

  const hasText = input.trim().length > 0;

  return (
    <div className="app-root">
      <div className="main-row">

        {/* ─── 左侧侧栏 ─── */}
        {sidebarOpen && (
          <div className="sidebar sidebar-left">
            <div className="sidebar-header">
              <div className="sidebar-title">Silences</div>
              <button className="sidebar-new-btn" onClick={newSession} title="新会话" style={{ display: 'none' }}>+</button>
            </div>

            <button className="new-chat-btn" onClick={newSession}>
              <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                <path d="M8 2.5v11M2.5 8h11" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round"/>
              </svg>
              开启新对话
            </button>

            <div className="sidebar-scroll">
              <div className="sidebar-nav">
                {sessions.length === 0 && (
                  <div style={{ padding: '24px 12px', textAlign: 'center', color: 'var(--ds-label-caption)', fontSize: 13 }}>
                    暂无会话
                  </div>
                )}
                {sessions.map(s => (
                  <div
                    key={s.id}
                    className={`session-item ${s.id === activeId ? 'active' : ''}`}
                    onClick={() => selectSession(s.id)}
                  >
                    <div className="session-avatar">💬</div>
                    <div className="session-info">
                      <div className="session-name">
                        {s.preview ? s.preview.slice(0, 24) + (s.preview.length > 24 ? '…' : '') : s.id.slice(0, 8) + '…'}
                      </div>
                      <div className="session-time">{fmtRelative(s.created_at)}</div>
                    </div>
                  </div>
                ))}
              </div>
            </div>

            {/* ─── 侧栏底部：设置 ─── */}
            <div className="sidebar-footer" onClick={() => setSettingsOpen(true)}>
              <div className="sidebar-footer-label">设置</div>
              <svg className="sidebar-footer-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                <circle cx="12" cy="12" r="3"/>
                <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/>
              </svg>
            </div>
          </div>
        )}

        {/* ─── 内容区 ─── */}
        <div className="content-area">
          <div className="chat-panel">

            {/* 顶部栏 */}
            <div style={{
              height: 60, display: 'flex', alignItems: 'center',
              padding: '0 24px', gap: 8, flexShrink: 0,
            }}>
              <span style={{ fontSize: 14, color: 'var(--ds-label-tertiary)', whiteSpace: 'nowrap', overflow: 'hidden', textOverflow: 'ellipsis' }}>
                {activeId
                  ? (sessions.find(s => s.id === activeId)?.preview?.slice(0, 40) || '会话')
                  : '新会话'
                }
              </span>
            </div>

            {/* 消息区 */}
            <div className="messages-scroll">
              <div className="messages-inner">
                {messages.length === 0 ? (
                  <div className="empty-state">
                    <div className="empty-state-icon">◇</div>
                    <div className="empty-state-title">开始新的对话</div>
                    <div className="empty-state-desc">
                      输入消息，与 Silences agent 协作编码
                    </div>
                  </div>
                ) : (
                  messages.map((msg, i) => (
                    <div key={i} className="message-row">
                      {msg.role === 'user' ? (
                        <div className="user-group">
                          <div className="user-bubble">{msg.content}</div>
                          <div className="msg-actions">
                            <button className="msg-copy-btn" onClick={() => { copyText(msg.content); setCopiedIdx(i); setTimeout(() => setCopiedIdx(null), 1000); }} title="复制">
                              {copiedIdx === i ? (
                                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                                  <path d="M3 8l3.5 3.5L13 4" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"/>
                                </svg>
                              ) : (
                                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                                  <rect x="4.5" y="5.5" width="9" height="10" rx="1.5" stroke="currentColor" strokeWidth="1.2"/>
                                  <path d="M11 4V3a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h1" stroke="currentColor" strokeWidth="1.2"/>
                                </svg>
                              )}
                            </button>
                          </div>
                        </div>
                      ) : (
                        <>
                          {msg.reasoning && (
                            <div className={`think-container ${collapsedThinking[i] ? 'collapsed' : ''}`}>
                              <div className="think-header" onClick={() => setCollapsedThinking(prev => ({ ...prev, [i]: !prev[i] }))}>
                                <svg className="think-icon" width="14" height="14" viewBox="0 0 24 24" fill="none">
                                  <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="1.5"/>
                                  <path d="M12 16v-4M12 8v.01" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"/>
                                </svg>
                                思考过程
                                <svg className="think-chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                                  {collapsedThinking[i]
                                    ? <path d="M9 18l6-6-6-6" />
                                    : <path d="M6 9l6 6 6-6" />
                                  }
                                </svg>
                              </div>
                              <div className="think-content"><Markdown>{msg.reasoning}</Markdown></div>
                            </div>
                          )}
                          {msg.toolCalls && msg.toolCalls.length > 0 && (
                            <div className="tc-group">
                              {msg.toolCalls.map((tc, tci) => {
                                const tcKey = `${i}-${tci}`;
                                const isCollapsed = collapsedToolCalls[tcKey] !== false;
                                return (
                                  <div key={tci} className={`tc-card ${isCollapsed ? 'collapsed' : ''}`}>
                                    <div className="tc-header" onClick={() => setCollapsedToolCalls(prev => ({ ...prev, [tcKey]: !isCollapsed }))}>
                                      <svg className="tc-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                                        <polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/>
                                      </svg>
                                      <span className="tc-name">{tc.name}</span>
                                      {tc.result && <span className="tc-status">✓</span>}
                                      <svg className="tc-chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                                        {isCollapsed ? <path d="M9 18l6-6-6-6" /> : <path d="M6 9l6 6 6-6" />}
                                      </svg>
                                    </div>
                                    <div className="tc-body">
                                      <div className="tc-args">{tc.args}</div>
                                      {tc.result && <div className="tc-result">{tc.result}</div>}
                                    </div>
                                  </div>
                                );
                              })}
                            </div>
                          )}
                          <div className="assistant-content" style={{ wordBreak: 'break-word' }}>
                            {msg.content ? (
                              <Markdown>{msg.content}</Markdown>
                            ) : msg.isStreaming ? (
                              <span className="thinking-dots"><span>.</span><span>.</span><span>.</span></span>
                            ) : '(空回复)'}
                          </div>
                          <div className="msg-actions">
                            <button className="msg-copy-btn" onClick={() => { copyText(msg.content); setCopiedIdx(i); setTimeout(() => setCopiedIdx(null), 1000); }} title="复制">
                              {copiedIdx === i ? (
                                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                                  <path d="M3 8l3.5 3.5L13 4" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"/>
                                </svg>
                              ) : (
                                <svg width="14" height="14" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                                  <rect x="4.5" y="5.5" width="9" height="10" rx="1.5" stroke="currentColor" strokeWidth="1.2"/>
                                  <path d="M11 4V3a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h1" stroke="currentColor" strokeWidth="1.2"/>
                                </svg>
                              )}
                            </button>
                          </div>
                        </>
                      )}
                    </div>
                  ))
                )}
                <div ref={msgEndRef} />
              </div>
            </div>

            {/* 声明栏 / 用量 */}
            <div className="disclaimer-bar">
              <div className="disclaimer-inner">
                <span className="usage-total">
                  {totalUsage
                    ? `↑${fmtNum(totalUsage.input_tokens)} ↓${fmtNum(totalUsage.output_tokens)} ${fmtCost(totalUsage.cost_yuan)}`
                    : '↑0 ↓0 ¥0'}
                </span>
                <span className="usage-cache">
                  {totalUsage && totalUsage.input_tokens > 0
                    ? `${Math.round(totalUsage.cache_hit_tokens / (totalUsage.input_tokens || 1) * 100)}% 缓存`
                    : '0% 缓存'}
                </span>
              </div>
            </div>

            {/* 输入区 */}
            <div className="input-section">
              <div className="input-mask" />
              <div className="input-container">
                <div className="input-text-wrap">
                  <textarea
                    ref={inputRef}
                    className="chat-input"
                    value={input}
                    onChange={e => setInput(e.target.value)}
                    onKeyDown={handleKeyDown}
                    placeholder="给 Silences 发送消息"
                    rows={2}
                    disabled={loading}
                    autoFocus
                  />
                </div>
                <div className="input-actions">
                  <div className="input-actions-left">
                    <button className="input-icon-btn" title="上传文件">
                      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                        <path d="M5.5498 9.75V5H6.9502V9.75C6.9502 10.3299 7.4201 10.7998 8 10.7998C8.5799 10.7998 9.0498 10.3299 9.0498 9.75V4.5C9.0498 2.9536 7.7964 1.7002 6.25 1.7002C4.7036 1.7002 3.4502 2.9536 3.4502 4.5V9.75C3.4502 12.2629 5.4871 14.2998 8 14.2998C10.5129 14.2998 12.5498 12.2629 12.5498 9.75V4H13.9502V9.75C13.9502 13.0361 11.2861 15.7002 8 15.7002C4.71391 15.7002 2.0498 13.0361 2.0498 9.75V4.5C2.04981 2.1804 3.9304 0.299806 6.25 0.299805C8.5696 0.299805 10.4502 2.1804 10.4502 4.5V9.75C10.4502 11.1031 9.3531 12.2002 8 12.2002C6.6469 12.2002 5.5498 11.1031 5.5498 9.75Z" fill="currentColor"/>
                      </svg>
                    </button>
                    <button className="input-icon-btn" title="语音输入">
                      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                        <rect x="6" y="1" width="4" height="10" rx="2" stroke="currentColor" strokeWidth="1.3"/>
                        <path d="M3 7C3 9.20914 4.79086 11 7 11H9C11.2091 11 13 9.20914 13 7" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"/>
                        <path d="M8 13V15" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round"/>
                      </svg>
                    </button>
                  </div>
                  <div className="input-actions-right">
                    <button
                      className={`send-btn ${hasText ? 'active' : ''}`}
                      onClick={sendMessage}
                      disabled={loading || !hasText}
                      title="发送"
                    >
                      <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                        <path d="M8.3125 0.981587C8.66767 1.0545 8.97902 1.20558 9.2627 1.43374C9.48724 1.61438 9.73029 1.85933 9.97949 2.10854L14.707 6.83608L13.293 8.25014L9 3.95717V15.0431H7V3.95717L2.70703 8.25014L1.29297 6.83608L6.02051 2.10854C6.26971 1.85933 6.51277 1.61438 6.7373 1.43374C6.97662 1.24126 7.28445 1.04542 7.6875 0.981587C7.8973 0.94841 8.1031 0.956564 8.3125 0.981587Z" fill="currentColor"/>
                      </svg>
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>

      </div>

      {/* ─── 设置遮罩 ─── */}
      {settingsOpen && (
        <div className="settings-overlay" onClick={() => setSettingsOpen(false)}>
          <div className="settings-modal" onClick={e => e.stopPropagation()}>
            <div className="settings-modal-header">
              <span>设置</span>
              <button className="settings-close-btn" onClick={() => setSettingsOpen(false)}>
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>
                </svg>
              </button>
            </div>
            <div className="settings-modal-body">
              <div className="settings-field">
                <label className="settings-label">API Key</label>
                <input
                  className="settings-input"
                  type="password"
                  placeholder={settings.api_key ? settings.api_key : '输入 DeepSeek API Key'}
                  value={settingsDirty.api_key ?? ''}
                  onChange={e => setSettingsDirty(prev => ({ ...prev, api_key: e.target.value }))}
                />
              </div>
              <div className="settings-field">
                <label className="settings-label">System Prompt</label>
                <textarea
                  className="settings-textarea"
                  placeholder="可选的系统提示词，用于设定 LLM 的行为和角色"
                  rows={5}
                  value={settingsDirty.system_prompt ?? ''}
                  onChange={e => setSettingsDirty(prev => ({ ...prev, system_prompt: e.target.value }))}
                />
              </div>
            </div>
            <div className="settings-modal-footer">
              <button className="settings-cancel-btn" onClick={() => {
                setSettingsDirty({ api_key: '', system_prompt: settings.system_prompt || '' });
                setSettingsOpen(false);
              }}>取消</button>
              <button className="settings-save-btn" onClick={async () => {
                await saveSettings();
                setSettingsOpen(false);
              }} disabled={settingsSaving}>
                {settingsSaving ? '保存中...' : '保存'}
              </button>
            </div>
          </div>
        </div>
      )}

    </div>
  );
}
