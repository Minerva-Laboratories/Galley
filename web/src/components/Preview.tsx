import { useEffect, useRef, useState } from 'preact/hooks';
import * as pdfjs from 'pdfjs-dist';
import type { PDFDocumentProxy } from 'pdfjs-dist';
import workerUrl from 'pdfjs-dist/build/pdf.worker.min.mjs?url';
import { api, ApiError } from '../api';
import { goToLine } from '../editor/Editor';
import { build as runBuild, showInPdf } from '../editor/commands';
import {
  build,
  draftMode,
  invertPdf,
  pdfVersion,
  project,
  requestGoto,
  showToast,
  syncTarget,
  toggleDraft,
  toggleInvert,
} from '../store/store';
import { Icon } from './Icon';

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

type Zoom = 'fit' | number;

export function Preview() {
  const host = useRef<HTMLDivElement>(null);
  const [doc, setDoc] = useState<PDFDocumentProxy | null>(null);
  const [pageCount, setPageCount] = useState(0);
  const [current, setCurrent] = useState(1);
  const [zoom, setZoom] = useState<Zoom>('fit');
  const [scale, setScale] = useState(1);
  const [highlight, setHighlight] = useState<{ page: number; top: number; height: number } | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const id = project.value?.id;
  const version = pdfVersion.value;
  const last = build.value.last;

  // Load (or reload) the document whenever a fresh PDF exists.
  useEffect(() => {
    if (!id || version === 0) {
      setDoc(null);
      setPageCount(0);
      return;
    }
    let cancelled = false;
    const task = pdfjs.getDocument({ url: api.pdfUrl(id, version) });
    task.promise
      .then((d) => {
        if (cancelled) {
          void d.destroy();
          return;
        }
        setDoc((prev) => {
          void prev?.destroy();
          return d;
        });
        setPageCount(d.numPages);
        setLoadError(null);
      })
      .catch((e: unknown) => {
        if (!cancelled) setLoadError(e instanceof Error ? e.message : 'Could not load the PDF.');
      });
    return () => {
      cancelled = true;
    };
  }, [id, version]);

  // Compute the scale from the pane width for "fit width".
  useEffect(() => {
    const el = host.current;
    if (!el || !doc) return;
    let stop = false;
    const compute = async () => {
      const page = await doc.getPage(1);
      if (stop) return;
      const width = page.getViewport({ scale: 1 }).width;
      const avail = el.clientWidth - 48;
      setScale(zoom === 'fit' ? Math.max(0.3, avail / width) : zoom);
    };
    void compute();
    const ro = new ResizeObserver(() => void compute());
    ro.observe(el);
    return () => {
      stop = true;
      ro.disconnect();
    };
  }, [doc, zoom]);

  // Scroll to a forward-sync target once the pages exist.
  const target = syncTarget.value;
  useEffect(() => {
    if (!target || !host.current) return;
    const pageEl = host.current.querySelector<HTMLElement>(`[data-page="${target.page}"]`);
    if (!pageEl) return;
    const top = target.y * scale;
    host.current.scrollTo({ top: pageEl.offsetTop + top - host.current.clientHeight / 3, behavior: 'smooth' });
    setHighlight({ page: target.page, top, height: Math.max(12, target.height * scale) });
    const t = setTimeout(() => setHighlight(null), 1800);
    return () => clearTimeout(t);
  }, [target?.nonce, scale, pageCount]);

  const onScroll = () => {
    const el = host.current;
    if (!el) return;
    const pages = Array.from(el.querySelectorAll<HTMLElement>('[data-page]'));
    const mid = el.scrollTop + el.clientHeight / 3;
    let page = 1;
    for (const p of pages) if (p.offsetTop <= mid) page = Number(p.dataset.page);
    setCurrent(page);
  };

  const onPageClick = async (e: MouseEvent, page: number) => {
    if (!id) return;
    const canvas = e.currentTarget as HTMLCanvasElement;
    const rect = canvas.getBoundingClientRect();
    const x = (e.clientX - rect.left) / scale;
    const y = (e.clientY - rect.top) / scale;
    try {
      const loc = await api.synctexInverse(id, page, x, y);
      const hit = requestGoto(loc.file, loc.line);
      if (hit) goToLine(hit.view, hit.line);
      showToast(`Jumped to ${loc.file}:${loc.line}`);
    } catch (err) {
      showToast(err instanceof ApiError ? err.message : 'No source location for that spot.');
    }
  };

  const stale = last?.status !== 'ok' && last?.status !== undefined && last.pdf_available;
  const showEmpty = version === 0;

  return (
    <section class="preview" aria-label="Preview">
      <div class="ph">
        <span style={{ fontWeight: 500 }}>Preview</span>
        <button class={`tb ${draftMode.value ? 'on' : ''}`} title="Skip figures for faster builds" onClick={() => (toggleDraft(), void runBuild())}>
          Draft
        </button>
        <button class={`tb ${invertPdf.value ? 'on' : ''}`} title="Dark page" onClick={toggleInvert}>
          Invert
        </button>
        <button class="tb" title="Show the cursor line in the PDF (Ctrl+click in the editor)" onClick={() => void showInPdf()} disabled={showEmpty}>
          Sync
        </button>
        <button class="tb" title="Zoom out" aria-label="Zoom out" onClick={() => setZoom((z) => Math.max(0.4, (z === 'fit' ? scale : z) - 0.15))} disabled={showEmpty}>
          −
        </button>
        <button class="tb" title="Fit width" onClick={() => setZoom('fit')} disabled={showEmpty}>
          Fit
        </button>
        <button class="tb" title="Zoom in" aria-label="Zoom in" onClick={() => setZoom((z) => Math.min(4, (z === 'fit' ? scale : z) + 0.15))} disabled={showEmpty}>
          +
        </button>
        <a
          class={`tb icon ${showEmpty ? 'disabled' : ''}`}
          title="Download PDF"
          aria-label="Download PDF"
          href={id && !showEmpty ? api.pdfUrl(id, version) : undefined}
          download={id ? `${id}.pdf` : undefined}
        >
          <Icon name="download" size={14} />
        </a>
        <span class="pg">{pageCount ? `${current} / ${pageCount}` : '— / —'}</span>
      </div>
      <div class={`pb ${showEmpty ? 'centered' : ''}`} ref={host} onScroll={onScroll}>
        {stale && <div class="stale">Last build failed. Showing the previous PDF.</div>}
        {showEmpty && (
          <div class="empty">
            <b>No PDF yet</b>
            {build.value.phase === 'running' ? 'Building…' : 'Press Ctrl+Enter to build.'}
          </div>
        )}
        {loadError && !showEmpty && <div class="err">{loadError}</div>}
        {doc &&
          Array.from({ length: pageCount }, (_, i) => (
            <Page
              key={`${version}:${i + 1}`}
              doc={doc}
              number={i + 1}
              scale={scale}
              invert={invertPdf.value}
              highlight={highlight?.page === i + 1 ? highlight : null}
              onClick={(e) => void onPageClick(e, i + 1)}
            />
          ))}
      </div>
    </section>
  );
}

function Page({
  doc,
  number,
  scale,
  invert,
  highlight,
  onClick,
}: {
  doc: PDFDocumentProxy;
  number: number;
  scale: number;
  invert: boolean;
  highlight: { top: number; height: number } | null;
  onClick: (e: MouseEvent) => void;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });

  useEffect(() => {
    let cancelled = false;
    let renderTask: { cancel: () => void } | null = null;
    void doc.getPage(number).then((page) => {
      if (cancelled || !canvas.current) return;
      const viewport = page.getViewport({ scale });
      const dpr = window.devicePixelRatio || 1;
      const c = canvas.current;
      c.width = Math.floor(viewport.width * dpr);
      c.height = Math.floor(viewport.height * dpr);
      c.style.width = `${Math.floor(viewport.width)}px`;
      c.style.height = `${Math.floor(viewport.height)}px`;
      setSize({ w: Math.floor(viewport.width), h: Math.floor(viewport.height) });
      const ctx = c.getContext('2d');
      if (!ctx) return;
      const task = page.render({ canvasContext: ctx, viewport, transform: dpr === 1 ? undefined : [dpr, 0, 0, dpr, 0, 0] });
      renderTask = task;
      task.promise.catch(() => {
        // Cancelled renders are expected when zoom changes quickly.
      });
    });
    return () => {
      cancelled = true;
      renderTask?.cancel();
    };
  }, [doc, number, scale]);

  return (
    <div class="pdf-page" data-page={number} style={{ width: size.w || undefined }}>
      <canvas ref={canvas} class={invert ? 'inv' : ''} onClick={onClick} title="Click to jump to the source" />
      {highlight && <div class="sync-hl" style={{ top: highlight.top - 2, height: highlight.height + 4 }} />}
    </div>
  );
}
