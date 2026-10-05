import { useEffect, useRef, useState } from 'preact/hooks';

const DEFAULT_WIDTH = 280;
const MIN_WIDTH = 240;
const MAX_WIDTH = 600;
const KEY = 'galley:drawer-width';

function preferredWidth() {
  try {
    const saved = Number(localStorage.getItem(KEY));
    return Number.isFinite(saved) && saved >= MIN_WIDTH ? Math.min(MAX_WIDTH, saved) : DEFAULT_WIDTH;
  } catch {
    return DEFAULT_WIDTH;
  }
}

/** Reserve room for the editor on desktop; the drawer overlays it on small screens. */
function availableWidth() {
  return Math.max(0, Math.min(MAX_WIDTH, window.innerWidth <= 700 ? window.innerWidth - 48 : window.innerWidth - 408));
}

export function useDrawerWidth() {
  const [width, setWidth] = useState(preferredWidth);
  const [limit, setLimit] = useState(availableWidth);
  useEffect(() => {
    const resize = () => setLimit(availableWidth());
    window.addEventListener('resize', resize);
    return () => window.removeEventListener('resize', resize);
  }, []);
  const change = (value: number) => {
    const next = Math.max(Math.min(MIN_WIDTH, limit), Math.min(limit, value));
    setWidth(next);
    try { localStorage.setItem(KEY, String(next)); } catch { /* Storage is optional. */ }
  };
  return { width: Math.min(width, limit), limit, change };
}

export function DrawerResize({ width, limit, change }: ReturnType<typeof useDrawerWidth>) {
  const drag = useRef<{ x: number; width: number } | null>(null);
  const [active, setActive] = useState(false);
  return (
    <div
      class={`drawer-resize ${active ? 'dragging' : ''}`}
      role="separator"
      tabIndex={0}
      aria-label="Resize sidebar"
      aria-orientation="vertical"
      aria-valuemin={Math.min(MIN_WIDTH, limit)}
      aria-valuemax={limit}
      aria-valuenow={Math.round(width)}
      title="Drag to resize. Double-click to reset."
      onPointerDown={(e) => {
        if (e.button !== 0) return;
        e.preventDefault();
        e.currentTarget.setPointerCapture(e.pointerId);
        drag.current = { x: e.clientX, width };
        setActive(true);
      }}
      onPointerMove={(e) => {
        if (drag.current) change(drag.current.width + e.clientX - drag.current.x);
      }}
      onPointerUp={(e) => {
        drag.current = null;
        setActive(false);
        if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId);
      }}
      onLostPointerCapture={() => { drag.current = null; setActive(false); }}
      onDblClick={() => change(DEFAULT_WIDTH)}
      onKeyDown={(e) => {
        if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(e.key)) return;
        e.preventDefault();
        change(e.key === 'Home' ? MIN_WIDTH : e.key === 'End' ? limit : width + (e.key === 'ArrowRight' ? 20 : -20));
      }}
    />
  );
}
