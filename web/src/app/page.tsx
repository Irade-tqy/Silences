'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Message, Session, AppSettings, SessionState, TokenUsage, ViewMessage } from '@/types';
import Sidebar from '@/components/Sidebar';
import ChatPanel from '@/components/ChatPanel';
import RightSidebar from '@/components/RightSidebar';
import SettingsModal from '@/components/SettingsModal';
import { ContextMenu, RenameModal, DeleteConfirmModal } from '@/components/Modals';
import { useViewport } from '@/hooks/useViewport';

export default function Page() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState('');
  const [loading, setLoading] = useState(false);
  const [paused, setPaused] = useState(false);
  const [totalUsage, setTotalUsage] = useState<TokenUsage | null>(null);
  const [roundUsage, setRoundUsage] = useState<TokenUsage | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [collapsedThinking, setCollapsedThinking] = useState<Record<number, boolean>>({});
  const [collapsedToolCalls, setCollapsedToolCalls] = useState<Record<string, boolean>>({});
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [settings, setSettings] = useState<AppSettings>({ api_key: null, system_prompt: null, warmup_enabled: true });
  const [settingsDirty, setSettingsDirty] = useState<AppSettings>({ api_key: '', system_prompt: '', warmup_enabled: true });
  const [settingsSaving, setSettingsSaving] = useState(false);
  const abortRef = useRef<AbortController | null>(null);
  const msgEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const thinkingIdxRef = useRef<number | null>(null);
  const [rightSidebarOpen, setRightSidebarOpen] = useState(false);
  const [sessionState, setSessionState] = useState<SessionState | null>(null);
  const [collapsedCtxCards, setCollapsedCtxCards] = useState<Record<number, boolean>>({});
  const [contextMenu, setContextMenu] = useState<{ id: string; x: number; y: number } | null>(null);
  const [renameId, setRenameId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState('');
  const [deleteConfirmId, setDeleteConfirmId] = useState<string | null>(null);
  const { isMobile } = useViewport();
  const [mobilePage, setMobilePage] = useState<'sessions' | 'chat' | 'right'>('sessions');

  const apiBase = useMemo(() => {
    const host = process.env.NEXT_PUBLIC_API_HOST ||
      (typeof window !== 'undefined' ? window.location.hostname : 'localhost');
    return `http://${host}:1030`;
  }, []);

  const scrollToBottom = useCallback(() => {
    const el = msgEndRef.current?.closest<HTMLElement>('.messages-scroll');
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  const loadSessions = useCallback(async () => {
    try {
      const res = await fetch(`${apiBase}/sessions`);
      if (res.ok) setSessions(await res.json());
    } catch { /* ignore */ }
  }, [apiBase]);

  useEffect(() => { loadSessions(); }, [loadSessions]);

  const settingsDirtyRef = useRef<AppSettings>({ api_key: '', system_prompt: '', warmup_enabled: true });
  useEffect(() => { settingsDirtyRef.current = settingsDirty; }, [settingsDirty]);

  const loadSettings = useCallback(async () => {
    try {
      const res = await fetch(`${apiBase}/settings?t=${Date.now()}`);
      if (res.ok) {
        const data: AppSettings = await res.json();
        console.log('GET /settings 响应:', data);
        setSettings(data);
        setSettingsDirty({ api_key: '', system_prompt: data.system_prompt || '', warmup_enabled: data.warmup_enabled });
      } else {
        console.warn('GET /settings 失败:', res.status);
      }
      setSettingsLoaded(true);
    } catch (e) {
      console.warn('加载设置失败:', e);
    }
  }, [apiBase]);

  const newSession = useCallback(() => {
    setActiveId(null);
    setMessages([]);
    setTotalUsage(null);
    setRoundUsage(null);
    setPaused(false);
    abortRef.current?.abort();
  }, []);

  // 轮询会话运行时状态（右侧边栏数据）
  useEffect(() => {
    if (!activeId) { setSessionState(null); return; }
    const fetchState = async () => {
      try {
        const res = await fetch(`${apiBase}/sessions/${activeId}/state`);
        if (res.ok) setSessionState(await res.json());
      } catch { /* ignore */ }
    };
    fetchState();
    const interval = setInterval(fetchState, loading ? 2000 : 5000);
    return () => clearInterval(interval);
  }, [activeId, loading, apiBase]);

  const handleContextMenu = useCallback((e: React.MouseEvent, sid: string) => {
    e.preventDefault();
    const menuW = 150;
    const menuH = 90;
    let x = e.clientX;
    let y = e.clientY;
    if (x + menuW > window.innerWidth) x = window.innerWidth - menuW - 8;
    if (y + menuH > window.innerHeight) y = y - menuH;
    setContextMenu({ id: sid, x, y });
  }, []);

  const handleRenameClick = useCallback((sid: string) => {
    setContextMenu(null);
    const s = sessions.find(s => s.id === sid);
    setRenameValue(s?.name || s?.preview || '');
    setRenameId(sid);
  }, [sessions]);

  const handleRenameConfirm = useCallback(async () => {
    if (!renameId) return;
    await fetch(`${apiBase}/sessions/${renameId}/rename`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: renameValue }),
    });
    setRenameId(null);
    loadSessions();
  }, [renameId, renameValue, loadSessions, apiBase]);

  const handleDeleteClick = useCallback((sid: string) => {
    setContextMenu(null);
    setDeleteConfirmId(sid);
  }, []);

  const handleDeleteConfirm = useCallback(async () => {
    if (!deleteConfirmId) return;
    await fetch(`${apiBase}/sessions/${deleteConfirmId}`, { method: 'DELETE' });
    if (activeId === deleteConfirmId) {
      setActiveId(null);
      setMessages([]);
      setTotalUsage(null);
    }
    setDeleteConfirmId(null);
    loadSessions();
  }, [deleteConfirmId, activeId, loadSessions, apiBase]);

  const saveSettings = useCallback(async () => {
    setSettingsSaving(true);
    try {
      const cur = settingsDirtyRef.current;
      const body: Record<string, string | null | boolean> = {};
      if (cur.api_key && cur.api_key.length > 0) {
        body.api_key = cur.api_key;
      }
      if (cur.system_prompt !== undefined) {
        body.system_prompt = cur.system_prompt || null;
      }
      body.warmup_enabled = cur.warmup_enabled;
      await fetch(`${apiBase}/settings`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      setSettingsDirty(prev => ({ ...prev, api_key: '' }));
      await loadSettings();
    } finally {
      setSettingsSaving(false);
    }
  }, [apiBase, loadSettings]);

  const selectSession = useCallback(async (id: string) => {
    setActiveId(id);
    setMessages([]);
    setTotalUsage(null);
    setRoundUsage(null);
    setPaused(false);
    abortRef.current?.abort();
    try {
      const res = await fetch(`${apiBase}/sessions/${id}/messages`);
      if (res.ok) {
        const view: ViewMessage[] = await res.json();
        const converted: Message[] = view.map(v => ({
          role: v.role as Message['role'],
          content: v.content,
          reasoning: v.reasoning_content,
          toolCalls: v.tool_calls?.map(tc => ({
            id: undefined,
            name: tc.name,
            args: tc.args,
            result: tc.result,
          })),
        }));
        setMessages(converted);
        if (isMobile) setMobilePage('chat');
      }
      const usageRes = await fetch(`${apiBase}/sessions/${id}/usage`);
      if (usageRes.ok) setTotalUsage(await usageRes.json());
    } catch { /* ignore */ }
  }, [apiBase, isMobile]);

  const sendMessage = useCallback(async () => {
    const text = input.trim();
    if (!text || loading) return;

    setInput('');
    setLoading(true);
    setPaused(false);
    setCollapsedToolCalls({});

    const userMsg: Message = { role: 'user', content: text };
    setMessages(prev => [...prev, userMsg]);

    const assistantMsg: Message = {
      role: 'assistant',
      content: '',
      isStreaming: true,
      reasoning: '',
    };
    setMessages(prev => [...prev, assistantMsg]);

    const ctrl = new AbortController();
    abortRef.current = ctrl;

    try {
      const res = await fetch(`${apiBase}/chat`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ message: text, session_id: activeId || null }),
        signal: ctrl.signal,
      });

      if (!res.ok) {
        setMessages(prev => {
          const copy = [...prev];
          const last = copy[copy.length - 1];
          if (last?.isStreaming) {
            last.content = `请求失败: ${res.status} ${res.statusText}`;
            last.isStreaming = false;
          }
          return copy;
        });
        setLoading(false);
        return;
      }

      const reader = res.body?.getReader();
      if (!reader) return;

      const decoder = new TextDecoder();
      let buffer = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          if (!line.startsWith('data: ')) continue;
          const data = line.slice(6).trim();
          if (!data) continue;

          try {
            const parsed = JSON.parse(data);

            if (parsed.type === 'session') {
              setActiveId(parsed.id);
              loadSessions();
            } else if (parsed.type === 'text') {
              setMessages(prev => {
                const copy = [...prev];
                const last = copy[copy.length - 1];
                if (last?.isStreaming) {
                  last.content += parsed.content;
                }
                return copy;
              });
            } else if (parsed.type === 'reasoning') {
              setMessages(prev => {
                const copy = [...prev];
                const last = copy[copy.length - 1];
                if (last?.isStreaming) {
                  last.reasoning = (last.reasoning || '') + parsed.content;
                }
                return copy;
              });
            } else if (parsed.type === 'tool_call') {
              if (parsed.result === null) {
                // Tool call start
                setMessages(prev => {
                  const copy = [...prev];
                  const last = copy[copy.length - 1];
                  if (last?.isStreaming) {
                    if (!last.toolCalls) last.toolCalls = [];
                    last.toolCalls.push({
                      id: parsed.id,
                      name: parsed.name,
                      args: parsed.args,
                    });
                  }
                  return copy;
                });
              } else {
                // Tool call end（更新结果）
                setMessages(prev => {
                  const copy = [...prev];
                  const last = copy[copy.length - 1];
                  if (last?.toolCalls) {
                    const tc = last.toolCalls.find(t => t.id === parsed.id);
                    if (tc) tc.result = parsed.result;
                  }
                  return copy;
                });
              }
            } else if (parsed.type === 'usage') {
              setRoundUsage({
                input_tokens: parsed.input_tokens,
                output_tokens: parsed.output_tokens,
                cache_hit_tokens: parsed.cache_hit_tokens,
                cache_miss_tokens: parsed.cache_miss_tokens,
                cost_yuan: parsed.cost_yuan,
              });
            } else if (parsed.type === 'paused') {
              setPaused(true);
            } else if (parsed.type === 'resumed') {
              setPaused(false);
            } else if (parsed.type === 'error') {
              setMessages(prev => {
                const copy = [...prev];
                const last = copy[copy.length - 1];
                if (last?.isStreaming) {
                  last.content = parsed.message;
                  last.isStreaming = false;
                }
                return copy;
              });
            } else if (parsed.type === 'message_boundary') {
              setMessages(prev => {
                const copy = [...prev];
                const last = copy[copy.length - 1];
                if (last?.isStreaming) last.isStreaming = false;
                return [...copy, { role: 'assistant', content: '', isStreaming: true, reasoning: '' }];
              });
            } else if (parsed.type === 'context_rollback') {
              setMessages(prev => {
                const copy = [...prev];
                const last = copy[copy.length - 1];
                if (last?.isStreaming) copy.pop();
                return [...copy, { role: 'assistant', content: '', isStreaming: true, reasoning: '' }];
              });
            }
          } catch { /* ignore parse errors */ }
        }
      }
    } catch (err: unknown) {
      if (err instanceof Error && err.name !== 'AbortError') {
        console.error('SSE stream error:', err);
      }
    } finally {
      setMessages(prev => {
        const copy = [...prev];
        const last = copy[copy.length - 1];
        if (last?.isStreaming) last.isStreaming = false;
        return copy;
      });
      setLoading(false);
      setPaused(false);
      loadSessions();
    }
  }, [input, loading, activeId, apiBase, loadSessions]);

  const stopGeneration = useCallback(async () => {
    if (!activeId) return;
    try {
      await fetch(`${apiBase}/sessions/${activeId}/set_state`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ action: 'stop' }),
      });
    } catch { /* ignore */ }
    abortRef.current?.abort();
    setPaused(false);
    setMessages(prev => {
      const copy = [...prev];
      const last = copy[copy.length - 1];
      if (last?.isStreaming) {
        last.isStreaming = false;
        if (!last.content && (!last.toolCalls || last.toolCalls.length === 0)) {
          last.content = '[已停止]';
        }
      }
      return copy;
    });
  }, [activeId, apiBase]);

  const pauseGeneration = useCallback(async () => {
    if (!activeId) return;
    // 立即更新 UI（不用等后端响应）
    setPaused(true);
    // 通知后端暂停（不 abort fetch，SSE 连接保持存活）
    try {
      await fetch(`${apiBase}/sessions/${activeId}/set_state`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ action: 'pause' }),
      });
    } catch { /* ignore network errors */ }
  }, [activeId, apiBase]);

  const resumeGeneration = useCallback(async () => {
    if (!activeId) return;
    // 立即更新 UI（不用等后端响应）
    setPaused(false);
    // 通知后端恢复（不创建新 fetch，现有 SSE 连接会收到 resumed 事件）
    try {
      await fetch(`${apiBase}/sessions/${activeId}/set_state`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ action: 'resume' }),
      });
    } catch { /* ignore */ }
  }, [activeId, apiBase]);

  return (
    <div className={`app-root${isMobile ? ' layout-mobile' : ''}`}>
      <div className="main-row">

        {/* ─── 左侧侧栏 ─── */}
        {sidebarOpen && (
          <Sidebar
            sessions={sessions}
            activeId={activeId}
            selectSession={selectSession}
            newSession={newSession}
            setSettingsOpen={setSettingsOpen}
            onContextMenu={handleContextMenu}
          />
        )}

        {/* ─── 内容区 ─── */}
        {/* 手机端由 mobile-page-stack 内的 ChatPanel 负责，不重复渲染避免 ref 冲突 */}
        {!isMobile && (
          <ChatPanel hideTopBar={false}
            messages={messages}
            input={input}
            setInput={setInput}
            loading={loading}
            paused={paused}
            totalUsage={totalUsage}
            roundUsage={roundUsage}
            activeId={activeId}
            sessions={sessions}
            rightSidebarOpen={rightSidebarOpen}
            setRightSidebarOpen={setRightSidebarOpen}
            sendMessage={sendMessage}
            stopGeneration={stopGeneration}
            pauseGeneration={pauseGeneration}
            resumeGeneration={resumeGeneration}
            collapsedThinking={collapsedThinking}
            setCollapsedThinking={setCollapsedThinking}
            collapsedToolCalls={collapsedToolCalls}
            setCollapsedToolCalls={setCollapsedToolCalls}
            inputRef={inputRef}
            msgEndRef={msgEndRef}
            scrollToBottom={scrollToBottom}
          />
        )}

        {/* ─── 右侧边栏 ─── */}
        <RightSidebar
          rightSidebarOpen={rightSidebarOpen}
          setRightSidebarOpen={setRightSidebarOpen}
          sessionState={sessionState}
          collapsedCtxCards={collapsedCtxCards}
          setCollapsedCtxCards={setCollapsedCtxCards}
        />

        {/* ─── 手机端页面栈 ─── */}
        {isMobile && (
          <div className="mobile-page-stack">
            {/* 会话列表页 */}
            <div className={`mobile-page ${mobilePage === 'sessions' ? 'active' : ''}`}>
              <div className="mobile-nav">
                <div className="mobile-nav-title">Silences</div>
              </div>
              <Sidebar
                sessions={sessions}
                activeId={activeId}
                selectSession={selectSession}
                newSession={newSession}
                setSettingsOpen={setSettingsOpen}
                onContextMenu={handleContextMenu}
              />
            </div>

            {/* 聊天页 */}
            <div className={`mobile-page ${mobilePage === 'chat' ? 'active' : ''}`}>
              <div className="mobile-nav">
                <button className="mobile-nav-back" onClick={() => setMobilePage('sessions')}>
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="15 18 9 12 15 6"/>
                  </svg>
                </button>
                <div className="mobile-nav-title">
                  {activeId
                    ? (sessions.find(s => s.id === activeId)?.preview?.slice(0, 24) || '会话')
                    : '新会话'}
                </div>
                <button className="mobile-nav-back" onClick={() => { setRightSidebarOpen(true); setMobilePage('right'); }}>
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                    <rect x="3" y="3" width="7" height="7" rx="1"/>
                    <rect x="14" y="3" width="7" height="7" rx="1"/>
                    <rect x="3" y="14" width="7" height="7" rx="1"/>
                    <rect x="14" y="14" width="7" height="7" rx="1"/>
                  </svg>
                </button>
              </div>
              <ChatPanel
                messages={messages}
                input={input}
                setInput={setInput}
                loading={loading}
                paused={paused}
                totalUsage={totalUsage}
                roundUsage={roundUsage}
                activeId={activeId}
                sessions={sessions}
                rightSidebarOpen={rightSidebarOpen}
                setRightSidebarOpen={setRightSidebarOpen}
                sendMessage={sendMessage}
                stopGeneration={stopGeneration}
                pauseGeneration={pauseGeneration}
                resumeGeneration={resumeGeneration}
                collapsedThinking={collapsedThinking}
                setCollapsedThinking={setCollapsedThinking}
                collapsedToolCalls={collapsedToolCalls}
                setCollapsedToolCalls={setCollapsedToolCalls}
                inputRef={inputRef}
                msgEndRef={msgEndRef}
                scrollToBottom={scrollToBottom}
                hideTopBar
              />
            </div>

            {/* 运行时面板页 */}
            <div className={`mobile-page ${mobilePage === 'right' ? 'active' : ''}`}>
              <div className="mobile-nav">
                <button className="mobile-nav-back" onClick={() => setMobilePage('chat')}>
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="15 18 9 12 15 6"/>
                  </svg>
                </button>
                <div className="mobile-nav-title">运行时视图</div>
              </div>
              <RightSidebar
                rightSidebarOpen={true}
                setRightSidebarOpen={setRightSidebarOpen}
                sessionState={sessionState}
                collapsedCtxCards={collapsedCtxCards}
                setCollapsedCtxCards={setCollapsedCtxCards}
              />
            </div>
          </div>
        )}

      </div>

      {/* ─── 设置遮罩 ─── */}
      <SettingsModal
        settingsOpen={settingsOpen}
        setSettingsOpen={setSettingsOpen}
        settings={settings}
        settingsDirty={settingsDirty}
        setSettingsDirty={setSettingsDirty}
        settingsSaving={settingsSaving}
        saveSettings={saveSettings}
        loadSettings={loadSettings}
      />

      {/* ─── 右键菜单 ─── */}
      <ContextMenu
        contextMenu={contextMenu}
        setContextMenu={setContextMenu}
        onRename={handleRenameClick}
        onDelete={handleDeleteClick}
      />

      {/* ─── 改名弹窗 ─── */}
      <RenameModal
        renameId={renameId}
        setRenameId={setRenameId}
        renameValue={renameValue}
        setRenameValue={setRenameValue}
        handleRenameConfirm={handleRenameConfirm}
      />

      {/* ─── 删除确认 ─── */}
      <DeleteConfirmModal
        deleteConfirmId={deleteConfirmId}
        setDeleteConfirmId={setDeleteConfirmId}
        handleDeleteConfirm={handleDeleteConfirm}
      />

    </div>
  );
}
