'use client';

import { useEffect } from 'react';
import { AppSettings } from '@/types';

interface SettingsModalProps {
  settingsOpen: boolean;
  setSettingsOpen: (v: boolean) => void;
  settings: AppSettings;
  settingsDirty: AppSettings;
  setSettingsDirty: (v: AppSettings | ((prev: AppSettings) => AppSettings)) => void;
  settingsSaving: boolean;
  saveSettings: () => void;
  loadSettings: () => void;
}

export default function SettingsModal({
  settingsOpen, setSettingsOpen,
  settings, settingsDirty, setSettingsDirty,
  settingsSaving, saveSettings, loadSettings,
}: SettingsModalProps) {
  useEffect(() => {
    if (settingsOpen) loadSettings();
  }, [settingsOpen, loadSettings]);

  if (!settingsOpen) return null;

  return (
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
          <div className="settings-field settings-toggle-row">
            <label className="settings-label">Prefix Cache 预热</label>
            <label className="toggle-switch">
              <input
                type="checkbox"
                checked={settingsDirty.warmup_enabled}
                onChange={e => setSettingsDirty(prev => ({ ...prev, warmup_enabled: e.target.checked }))}
              />
              <span className="toggle-slider"></span>
            </label>
            <span className="settings-toggle-hint">
              {settingsDirty.warmup_enabled ? '启用（~2s 延迟）' : '关闭'}
            </span>
          </div>
          <div className="settings-field settings-toggle-row">
            <label className="settings-label">自动清理上下文</label>
            <label className="toggle-switch">
              <input
                type="checkbox"
                checked={settingsDirty.auto_collapse_prev}
                onChange={e => setSettingsDirty(prev => ({ ...prev, auto_collapse_prev: e.target.checked }))}
              />
              <span className="toggle-slider"></span>
            </label>
            <span className="settings-toggle-hint">
              {settingsDirty.auto_collapse_prev ? '去思考/过滤失败调用/精简结果' : '关闭'}
            </span>
          </div>
        </div>
        <div className="settings-modal-footer">
          <button className="settings-cancel-btn" onClick={() => {
            setSettingsDirty({ api_key: '', system_prompt: settings.system_prompt || '', warmup_enabled: settings.warmup_enabled, auto_collapse_prev: settings.auto_collapse_prev });
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
  );
}
