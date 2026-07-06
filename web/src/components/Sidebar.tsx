'use client';

import { Session } from '@/types';
import { fmtRelative } from '@/utils';

interface SidebarProps {
  sessions: Session[];
  activeId: string | null;
  selectSession: (id: string) => void;
  newSession: () => void;
  setSettingsOpen: (v: boolean) => void;
  onContextMenu: (e: React.MouseEvent, sid: string) => void;
}

export default function Sidebar({
  sessions, activeId, selectSession, newSession,
  setSettingsOpen, onContextMenu,
}: SidebarProps) {
  return (
    <div className="sidebar sidebar-left">
      <div className="sidebar-header">
        <div className="sidebar-title">Silences</div>
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
              onContextMenu={(e) => onContextMenu(e, s.id)}
            >
              <div className="session-avatar">💬</div>
              <div className="session-info">
                <div className="session-name">
                  {s.name || (s.preview ? s.preview.slice(0, 24) + (s.preview.length > 24 ? '…' : '') : s.id.slice(0, 8) + '…')}
                </div>
                <div className="session-time">{fmtRelative(s.created_at)}</div>
              </div>
            </div>
          ))}
        </div>
      </div>

      <div className="sidebar-footer" onClick={() => setSettingsOpen(true)}>
        <div className="sidebar-footer-label">设置</div>
        <svg className="sidebar-footer-icon" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="12" cy="12" r="3"/>
          <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z"/>
        </svg>
      </div>
    </div>
  );
}
