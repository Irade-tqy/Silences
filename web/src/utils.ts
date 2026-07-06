export function fmtTime(iso: string) {
  const d = new Date(iso);
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function fmtRelative(iso: string): string {
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

export function fmtCost(yuan: number) {
  if (yuan < 0.0001) return '¥0';
  return `¥${yuan.toFixed(3)}`;
}

export function copyText(text: string) {
  navigator.clipboard.writeText(text).catch(e => console.warn('复制到剪贴板失败:', e));
}

export function fmtNum(n: number): string {
  if (n >= 1_000_000_000_000) return (n / 1_000_000_000_000).toFixed(1) + 't';
  if (n >= 1_000_000_000) return (n / 1_000_000_000).toFixed(1) + 'b';
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + 'm';
  if (n >= 1_000) return (n / 1_000).toFixed(1) + 'k';
  return n.toString();
}

export function truncateContent(text: string, max: number): string {
  if (text.length <= max) return text;
  return text.slice(0, max) + '...';
}
