'use client';

import { SessionState } from '@/types';
import SurgeryPanel from './SurgeryPanel';

interface RightSidebarProps {
  rightSidebarOpen: boolean;
  setRightSidebarOpen: (v: boolean) => void;
  sessionState: SessionState | null;
  collapsedCtxCards: Record<number, boolean>;
  setCollapsedCtxCards: (v: Record<number, boolean> | ((prev: Record<number, boolean>) => Record<number, boolean>)) => void;
  apiBase: string;
  activeId: string | null;
  onContextUpdated?: () => void;
}

export default function RightSidebar({
  rightSidebarOpen, setRightSidebarOpen,
  sessionState, collapsedCtxCards, setCollapsedCtxCards,
  apiBase, activeId, onContextUpdated,
}: RightSidebarProps) {
  if (!rightSidebarOpen) return null;

  return (
    <div className="sidebar sidebar-right">
      <div className="sidebar-header">
        <div className="sidebar-title">运行时视图</div>
        <button className="sidebar-close-btn" onClick={() => setRightSidebarOpen(false)}>✕</button>
      </div>
      <div className="sidebar-scroll">

        {/* 手术刀上下文管理 */}
        <SurgeryPanel
          sessionState={sessionState}
          apiBase={apiBase}
          activeId={activeId}
          collapsedCtxCards={collapsedCtxCards}
          setCollapsedCtxCards={setCollapsedCtxCards}
          onContextUpdated={onContextUpdated}
        />

      </div>
    </div>
  );
}
