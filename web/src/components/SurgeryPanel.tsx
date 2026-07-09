'use client';

import { useState, useRef } from 'react';
import { SessionState, RawMessage } from '@/types';
import { truncateContent } from '@/utils';

interface SurgeryPanelProps {
  sessionState: SessionState | null;
  apiBase: string;
  activeId: string | null;
  collapsedCtxCards: Record<number, boolean>;
  setCollapsedCtxCards: (v: Record<number, boolean> | ((prev: Record<number, boolean>) => Record<number, boolean>)) => void;
  onContextUpdated?: () => void;
}

export default function SurgeryPanel({
  sessionState, apiBase, activeId,
  collapsedCtxCards, setCollapsedCtxCards,
  onContextUpdated,
}: SurgeryPanelProps) {
  const [surgeryPrompt, setSurgeryPrompt] = useState('');
  const [surgeryLoading, setSurgeryLoading] = useState(false);
  const [surgeryWaiting, setSurgeryWaiting] = useState(false);
  const [surgeryError, setSurgeryError] = useState<string | null>(null);
  const abortRef = useRef<AbortController | null>(null);

  const handleSurgery = async () => {
    const text = surgeryPrompt.trim();
    if (!text || !activeId || surgeryLoading) return;

    setSurgeryLoading(true);
    setSurgeryWaiting(false);
    setSurgeryError(null);
    setSurgeryPrompt('');

    const ctrl = new AbortController();
    abortRef.current = ctrl;

    try {
      const res = await fetch(`${apiBase}/sessions/${activeId}/surgery`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ prompt: text }),
        signal: ctrl.signal,
      });

      if (!res.ok) {
        setSurgeryError(`请求失败: ${res.status} ${res.statusText}`);
        setSurgeryLoading(false);
        return;
      }

      const reader = res.body?.getReader();
      if (!reader) {
        setSurgeryLoading(false);
        return;
      }

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
          const data = line.replace(/^data:\s*/, '').trim();
          if (!data) continue;

          try {
            const parsed = JSON.parse(data);

            if (parsed.type === 'tool_call') {
              if (parsed.name === 'wait') {
                if (parsed.result === null) {
                  setSurgeryWaiting(true);
                } else {
                  setSurgeryWaiting(false);
                }
              }
              // tool_call with result → 触发上下文刷新
              if (parsed.result !== null) {
                onContextUpdated?.();
              }
            } else if (parsed.type === 'error') {
              setSurgeryError(parsed.message);
              setSurgeryLoading(false);
              setSurgeryWaiting(false);
            }
          } catch { /* ignore parse errors */ }
        }
      }
    } catch (err: unknown) {
      if (err instanceof Error && err.name !== 'AbortError') {
        setSurgeryError(err.message);
      }
    } finally {
      setSurgeryLoading(false);
      setSurgeryWaiting(false);
    }
  };

  const handleClearContext = async () => {
    if (!activeId) return;
    try {
      const res = await fetch(`${apiBase}/sessions/${activeId}/state`);
      if (res.ok) {
        // 对于清空上下文，目前先通过手术刀 Agent 发送"清空所有消息"
        setSurgeryPrompt('清空所有消息');
        // 自动触发手术
        setTimeout(() => {
          const btn = document.getElementById('surgery-send-btn');
          btn?.click();
        }, 100);
      }
    } catch { /* ignore */ }
  };

  const handleCancel = async () => {
    if (!activeId) return;
    abortRef.current?.abort();
    setSurgeryLoading(false);
    setSurgeryWaiting(false);
    try {
      await fetch(`${apiBase}/sessions/${activeId}/set_state`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ action: 'stop' }),
      });
    } catch { /* ignore */ }
  };

  const context = sessionState?.context ?? [];

  return (
    <div className="right-panel-section surgery-panel">
      <div className="right-panel-title">上下文操作</div>

      {/* 输入区 */}
      <div className="surgery-input-area">
        <input
          className="surgery-input"
          type="text"
          value={surgeryPrompt}
          onChange={e => setSurgeryPrompt(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleSurgery(); } }}
          placeholder="输入指令修改上下文…"
          disabled={surgeryLoading}
        />
        <div className="surgery-actions">
          <button
            id="surgery-send-btn"
            className="surgery-btn surgery-btn-primary"
            onClick={handleSurgery}
            disabled={!surgeryPrompt.trim() || surgeryLoading || !activeId}
          >
            执行
          </button>
          <button
            className="surgery-btn surgery-btn-secondary"
            onClick={handleCancel}
            disabled={!surgeryLoading}
          >
            取消
          </button>
        </div>
      </div>

      {/* 状态提示 */}
      {surgeryWaiting && (
        <div className="surgery-status">⏳ 等待条件中…</div>
      )}
      {surgeryLoading && !surgeryWaiting && (
        <div className="surgery-status">⟳ 操作中…</div>
      )}
      {surgeryError && (
        <div className="surgery-status surgery-error">{surgeryError}</div>
      )}

      {/* 上下文消息列表 */}
      <div className="surgery-context-list">
        {context.length === 0 ? (
          <div className="right-empty">暂无上下文消息</div>
        ) : (
          context.map((msg, i) => {
            const collapsed = collapsedCtxCards[i] !== false;
            const label = msg.tool_calls && msg.tool_calls.length > 0
              ? msg.tool_calls[0].function.name + (msg.tool_calls.length > 1 ? ` +${msg.tool_calls.length - 1}` : '')
              : msg.role === 'tool'
                ? msg.name || 'tool'
                : msg.name ? `${msg.role} @${msg.name}` : msg.role;
            return (
              <div key={i} className={`ctx-card ${collapsed ? 'collapsed' : ''}`}>
                <div className="ctx-card-header" onClick={() => setCollapsedCtxCards(p => ({...p, [i]: !collapsed}))}>
                  <span className="ctx-card-role-badge" data-role={msg.role}>{label}</span>
                  <svg className="ctx-card-chevron" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    {collapsed ? <path d="M9 18l6-6-6-6"/> : <path d="M6 9l6 6 6-6"/>}
                  </svg>
                </div>
                <div className="ctx-card-body">
                  <pre className="ctx-card-content">{truncateContent(msg.content, 200)}</pre>
                  {msg.reasoning_content && <pre className="ctx-card-reasoning">reasoning: {truncateContent(msg.reasoning_content, 100)}</pre>}
                  {msg.tool_calls && msg.tool_calls.length > 0 && (
                    <div className="ctx-card-toolcalls">
                      {(msg.tool_calls as Array<{function: {name: string}}>).map((tc, tci) => (
                        <code key={tci}>{tc.function.name}</code>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
