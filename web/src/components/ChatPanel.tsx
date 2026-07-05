'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import { Message, Session, TokenUsage } from '@/types';
import { copyText, fmtCost, fmtNum } from '@/utils';
import Markdown from '@/components/Markdown';

interface ChatPanelProps {
  messages: Message[];
  input: string;
  setInput: (v: string) => void;
  loading: boolean;
  paused: boolean;
  totalUsage: TokenUsage | null;
  roundUsage: TokenUsage | null;
  activeId: string | null;
  sessions: Session[];
  rightSidebarOpen: boolean;
  setRightSidebarOpen: (v: boolean | ((prev: boolean) => boolean)) => void;
  sendMessage: () => void;
  stopGeneration: () => void;
  pauseGeneration: () => void;
  resumeGeneration: () => void;
  collapsedThinking: Record<number, boolean>;
  setCollapsedThinking: (v: Record<number, boolean> | ((prev: Record<number, boolean>) => Record<number, boolean>)) => void;
  collapsedToolCalls: Record<string, boolean>;
  setCollapsedToolCalls: (v: Record<string, boolean> | ((prev: Record<string, boolean>) => Record<string, boolean>)) => void;
  inputRef: React.RefObject<HTMLTextAreaElement | null>;
  msgEndRef: React.RefObject<HTMLDivElement | null>;
  scrollToBottom: () => void;
  hideTopBar?: boolean;
}

export default function ChatPanel({
  messages, input, setInput, loading, paused,
  totalUsage, activeId, sessions,
  rightSidebarOpen, setRightSidebarOpen,
  sendMessage, stopGeneration, pauseGeneration, resumeGeneration,
  collapsedThinking, setCollapsedThinking,
  collapsedToolCalls, setCollapsedToolCalls,
  inputRef, msgEndRef, scrollToBottom, hideTopBar,
}: ChatPanelProps) {
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);

  useEffect(() => { scrollToBottom(); }, [messages, scrollToBottom]);

  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      sendMessage();
    }
  }, [sendMessage]);

  const hasText = input.trim().length > 0;

  return (
    <div className="chat-panel">
      {!hideTopBar && (
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
          <button className="sidebar-toggle-right" onClick={() => setRightSidebarOpen(v => !v)} title="上下文面板">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
              <rect x="3" y="3" width="7" height="7" rx="1"/>
              <rect x="14" y="3" width="7" height="7" rx="1"/>
              <rect x="3" y="14" width="7" height="7" rx="1"/>
              <rect x="14" y="14" width="7" height="7" rx="1"/>
            </svg>
          </button>
        </div>
      )}

      {/* Messages area */}
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
                    <div className="assistant-content" style={{ wordBreak: 'break-word' }}>
                      {msg.content ? (
                        <Markdown>{msg.content}</Markdown>
                      ) : msg.isStreaming && (!msg.toolCalls || msg.toolCalls.length === 0) ? (
                        <span className="thinking-dots"><span>.</span><span>.</span><span>.</span></span>
                      ) : null}
                    </div>
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

      {/* Disclaimer / Usage */}
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

      {/* Input area */}
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
              {loading && paused ? (
                <>
                  <button
                    className="send-btn active"
                    onClick={resumeGeneration}
                    title="继续"
                  >
                    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                      <polygon points="4,2 14,8 4,14" fill="currentColor"/>
                    </svg>
                  </button>
                  <button
                    className="send-btn active stop-btn"
                    onClick={stopGeneration}
                    title="停止"
                  >
                    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                      <rect x="3" y="3" width="10" height="10" rx="2" fill="currentColor"/>
                    </svg>
                  </button>
                </>
              ) : (
                <button
                  className={`send-btn ${hasText || loading ? 'active' : ''}`}
                  onClick={loading ? pauseGeneration : sendMessage}
                  disabled={!hasText && !loading}
                  title={loading ? '暂停' : '发送'}
                >
                  {loading ? (
                    <svg width="16" height="16" viewBox="0 0 16 16" fill="none">
                      <rect x="3" y="2" width="3.5" height="12" rx="1" fill="currentColor"/>
                      <rect x="9.5" y="2" width="3.5" height="12" rx="1" fill="currentColor"/>
                    </svg>
                  ) : (
                    <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
                      <path d="M8.3125 0.981587C8.66767 1.0545 8.97902 1.20558 9.2627 1.43374C9.48724 1.61438 9.73029 1.85933 9.97949 2.10854L14.707 6.83608L13.293 8.25014L9 3.95717V15.0431H7V3.95717L2.70703 8.25014L1.29297 6.83608L6.02051 2.10854C6.26971 1.85933 6.51277 1.61438 6.7373 1.43374C6.97662 1.24126 7.28445 1.04542 7.6875 0.981587C7.8973 0.94841 8.1031 0.956564 8.3125 0.981587Z" fill="currentColor"/>
                    </svg>
                  )}
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
