'use client';

import { memo } from 'react';
import MarkdownIt from 'markdown-it';

const md = new MarkdownIt({
  html: false,
  linkify: true,
  typographer: true,
  breaks: false,  // 标准 markdown：双换行分段落，单换行视为空格
});

// 外部链接 target="_blank"
const linkOpenDefault =
  md.renderer.rules.link_open ??
  ((tokens, idx, options, env, self) => self.renderToken(tokens, idx, options));

md.renderer.rules.link_open = (tokens, idx, options, env, self) => {
  const token = tokens[idx];
  token.attrSet('target', '_blank');
  token.attrSet('rel', 'noopener noreferrer');
  return linkOpenDefault(tokens, idx, options, env, self);
};

interface MarkdownProps {
  children: string;
}

/**
 * Markdown 渲染组件（markdown-it）
 * - 自动链接、表格、删除线
 * - 外部链接在新标签页打开
 * - 样式由全局 CSS 控制 (.assistant-content / .think-content)
 */
function Markdown({ children }: MarkdownProps) {
  const html = md.render(children);
  return <div className="md-html" dangerouslySetInnerHTML={{ __html: html }} />;
}

export default memo(Markdown);
