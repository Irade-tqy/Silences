'use client';

import { SessionState } from '@/types';
import { truncateContent } from '@/utils';

interface RightSidebarProps {
  rightSidebarOpen: boolean;
  setRightSidebarOpen: (v: boolean) => void;
  sessionState: SessionState | null;
  collapsedCtxCards: Record<number, boolean>;
  setCollapsedCtxCards: (v: Record<number, boolean> | ((prev: Record<number, boolean>) => Record<number, boolean>)) => void;
}

export default function RightSidebar({
  rightSidebarOpen, setRightSidebarOpen,
  sessionState, collapsedCtxCards, setCollapsedCtxCards,
}: RightSidebarProps) {
  if (!rightSidebarOpen) return null;

  return (
    <div className="sidebar sidebar-right">
      <div className="sidebar-header">
        <div className="sidebar-title">运行时视图</div>
        <button className="sidebar-close-btn" onClick={() => setRightSidebarOpen(false)}>✕</button>
      </div>
      <div className="sidebar-scroll">

        <div className="right-panel-section">
          <div className="right-panel-title">
            模型上下文 ({sessionState?.context.length ?? 0} 条消息)
          </div>
          {(sessionState?.context ?? []).map((msg, i) => {
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
                      {msg.tool_calls.map(tc => <code key={tc.id}>{tc.function.name}</code>)}
                    </div>
                  )}
                </div>
              </div>
            );
          })}
          {(!sessionState || sessionState.context.length === 0) && (
            <div className="right-empty">暂无上下文快照</div>
          )}
        </div>

        <div className="right-panel-section">
          <div className="right-panel-title">
            检查点 ({(sessionState?.checkpoints ?? []).length})
          </div>
          {(sessionState?.checkpoints ?? []).map((cp, i) => (
            <div className="task-item" key={cp.id}>
              <span className="task-id">{i + 1}. {cp.id}</span>
              <span className="task-desc">{cp.description}</span>
            </div>
          ))}
          {(!sessionState || sessionState.checkpoints.length === 0) && (
            <div className="right-empty">暂无检查点</div>
          )}
        </div>

      </div>
    </div>
  );
}
