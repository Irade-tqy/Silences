'use client';

import { useCallback, useEffect, useRef, useState } from 'react';

export interface ViewportInfo {
  width: number;
  height: number;
  aspectRatio: number;
  isMobile: boolean;
}

function getViewport(): ViewportInfo {
  if (typeof window === 'undefined') {
    return { width: 1920, height: 1080, aspectRatio: 16 / 9, isMobile: false };
  }
  const w = window.innerWidth;
  const h = window.innerHeight;
  const ratio = w / h;
  /* 手机判定：宽度 < 640px 无条件手机；或宽高比 < 0.9（竖屏强迫）且宽度 < 1024px */
  const isMobile = w < 640 || (ratio < 0.9 && w < 1024);
  return { width: w, height: h, aspectRatio: ratio, isMobile };
}

export function useViewport(): ViewportInfo {
  const [vp, setVp] = useState<ViewportInfo>(getViewport);
  const timeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const handleResize = useCallback(() => {
    if (timeoutRef.current) {
      clearTimeout(timeoutRef.current);
    }
    timeoutRef.current = setTimeout(() => {
      setVp(getViewport());
    }, 100);
  }, []);

  useEffect(() => {
    window.addEventListener('resize', handleResize);
    return () => {
      window.removeEventListener('resize', handleResize);
      if (timeoutRef.current) clearTimeout(timeoutRef.current);
    };
  }, [handleResize]);

  return vp;
}
