import { useEffect, useRef, useState } from 'preact/hooks';
import * as pdfjs from 'pdfjs-dist';
import type { PDFDocumentLoadingTask, RenderTask } from 'pdfjs-dist';
import workerUrl from 'pdfjs-dist/build/pdf.worker.min.mjs?url';
import { api } from '../api';
import { Icon } from './Icon';

pdfjs.GlobalWorkerOptions.workerSrc = workerUrl;

/** Load only visible cards, and only the last successful PDF. Opening the dashboard never builds. */
export function ProjectThumbnail({ id, version }: { id: string; version: string }) {
  const host = useRef<HTMLDivElement>(null);
  const canvas = useRef<HTMLCanvasElement>(null);
  const [visible, setVisible] = useState(false);
  const [status, setStatus] = useState<'loading' | 'ready' | 'empty' | 'error'>('loading');

  useEffect(() => {
    if (!host.current) return;
    const observer = new IntersectionObserver(([entry]) => {
      if (entry?.isIntersecting) {
        setVisible(true);
        observer.disconnect();
      }
    }, { rootMargin: '100px' });
    observer.observe(host.current);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!visible) return;
    let cancelled = false;
    let loading: PDFDocumentLoadingTask | undefined;
    let rendering: RenderTask | undefined;
    setStatus('loading');
    void (async () => {
      try {
        loading = pdfjs.getDocument({ url: api.pdfUrl(id, Date.parse(version) || 0), disableAutoFetch: true });
        const doc = await loading.promise;
        if (cancelled) return;
        const page = await doc.getPage(1);
        const el = canvas.current;
        if (cancelled || !el) return;
        const viewport = page.getViewport({ scale: 540 / page.getViewport({ scale: 1 }).width });
        el.width = Math.ceil(viewport.width);
        el.height = Math.ceil(viewport.height);
        rendering = page.render({ canvasContext: el.getContext('2d')!, viewport });
        await rendering.promise;
        if (!cancelled) setStatus('ready');
      } catch (error) {
        if (!cancelled) {
          const missing = error instanceof Error && (error.name === 'MissingPDFException' || ('status' in error && error.status === 404));
          setStatus(missing ? 'empty' : 'error');
        }
      } finally {
        if (loading) void loading.destroy().catch(() => {});
      }
    })();
    return () => {
      cancelled = true;
      rendering?.cancel();
      if (loading) void loading.destroy().catch(() => {});
    };
  }, [id, version, visible]);

  return (
    <div class={`project-thumbnail ${status}`} ref={host}>
      <canvas ref={canvas} role="img" aria-label="First page of the last compiled PDF" hidden={status !== 'ready'} />
      {status !== 'ready' && <span class="project-thumbnail-placeholder"><Icon name="file-pdf" size={26} />
        {status === 'loading' ? 'Loading preview…' : status === 'empty' ? 'Build once to show a preview' : 'Preview unavailable'}
      </span>}
    </div>
  );
}
