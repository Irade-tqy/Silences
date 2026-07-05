'use client';

import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { Session } from '@/types';

// ─── ContextMenu ───

interface ContextMenuProps {
  contextMenu: { id: string; x: number; y: number } | null;
  setContextMenu: (v: { id: string; x: number; y: number } | null) => void;
  onRename: (sid: string) => void;
  onDelete: (sid: string) => void;
}

export function ContextMenu({ contextMenu, setContextMenu, onRename, onDelete }: ContextMenuProps) {
  const contextMenuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!contextMenu) return;
    const handleClick = (e: MouseEvent) => {
      if (contextMenuRef.current && !contextMenuRef.current.contains(e.target as Node)) {
        setContextMenu(null);
      }
    };
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [contextMenu, setContextMenu]);

  const [mounted, setMounted] = useState(false);
  useEffect(() => { setMounted(true); }, []);

  if (!contextMenu || !mounted) return null;

  return createPortal(
    <div className="ctx-menu" ref={contextMenuRef} style={{ left: contextMenu.x, top: contextMenu.y }}>
      <div className="ctx-item" onClick={() => onRename(contextMenu.id)}>重命名</div>
      <div className="ctx-item ctx-danger" onClick={() => onDelete(contextMenu.id)}>删除</div>
    </div>,
    document.body
  );
}

// ─── RenameModal ───

interface RenameModalProps {
  renameId: string | null;
  setRenameId: (v: string | null) => void;
  renameValue: string;
  setRenameValue: (v: string) => void;
  handleRenameConfirm: () => void;
}

export function RenameModal({ renameId, setRenameId, renameValue, setRenameValue, handleRenameConfirm }: RenameModalProps) {
  if (!renameId) return null;

  return (
    <div className="settings-overlay" onClick={() => setRenameId(null)}>
      <div className="settings-modal" onClick={e => e.stopPropagation()} style={{ width: 380 }}>
        <div className="settings-modal-header">
          <span>重命名会话</span>
          <button className="settings-close-btn" onClick={() => setRenameId(null)}>
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/>
            </svg>
          </button>
        </div>
        <div className="settings-modal-body">
          <input
            className="settings-input"
            type="text"
            value={renameValue}
            onChange={e => setRenameValue(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') handleRenameConfirm(); }}
            autoFocus
          />
        </div>
        <div className="settings-modal-footer">
          <button className="settings-cancel-btn" onClick={() => setRenameId(null)}>取消</button>
          <button className="settings-save-btn" onClick={handleRenameConfirm}>确定</button>
        </div>
      </div>
    </div>
  );
}

// ─── DeleteConfirmModal ───

interface DeleteConfirmModalProps {
  deleteConfirmId: string | null;
  setDeleteConfirmId: (v: string | null) => void;
  handleDeleteConfirm: () => void;
}

export function DeleteConfirmModal({ deleteConfirmId, setDeleteConfirmId, handleDeleteConfirm }: DeleteConfirmModalProps) {
  if (!deleteConfirmId) return null;

  return (
    <div className="settings-overlay" onClick={() => setDeleteConfirmId(null)}>
      <div className="settings-modal" onClick={e => e.stopPropagation()} style={{ width: 380 }}>
        <div className="settings-modal-header">
          <span>删除会话</span>
        </div>
        <div className="settings-modal-body">
          <p style={{ color: 'var(--ds-label-secondary)', margin: 0 }}>确定要删除该会话吗？此操作不可撤销。</p>
        </div>
        <div className="settings-modal-footer">
          <button className="settings-cancel-btn" onClick={() => setDeleteConfirmId(null)}>取消</button>
          <button className="settings-save-btn" onClick={handleDeleteConfirm} style={{ background: 'var(--ds-state-error)' }}>删除</button>
        </div>
      </div>
    </div>
  );
}
