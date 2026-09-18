import { useEffect, useRef, useState } from 'preact/hooks';
import { api, ApiError, type Candidate, type CitationGraph as Graph, type CitationNode } from '../api';
import { goToLine } from '../editor/Editor';
import { graphOpen, project, requestGoto, showToast } from '../store/store';

/** A node in the simulation: a bibliography entry, or a candidate the catalogues suggested. */
interface Point {
  key: string;
  /** Section numbers citing this entry, for the section filter. */
  sections: string[];
  label: string;
  title: string;
  detail: string;
  cited: number;
  issues: string[];
  candidate: boolean;
  entry?: CitationNode;
  doi?: string | null;
  x: number;
  y: number;
  vx: number;
  vy: number;
  r: number;
}

interface Link {
  a: number;
  b: number;
  weight: number;
  candidate: boolean;
}

const TICKS = 320;

/** Deterministic placement: the same bibliography always draws the same way. */
function seededAngle(i: number, n: number): number {
  return (i / Math.max(1, n)) * Math.PI * 2;
}

function build(graph: Graph, candidates: Candidate[], width: number, height: number): { points: Point[]; links: Link[] } {
  const points: Point[] = graph.nodes.map((n, i) => {
    const a = seededAngle(i, graph.nodes.length);
    const spread = Math.min(width, height) / 3;
    return {
      key: n.key,
      sections: n.sections,
      label: n.key,
      title: n.title ?? n.key,
      detail: [n.year, n.venue].filter(Boolean).join(' · '),
      cited: n.cited,
      issues: n.issues,
      candidate: false,
      entry: n,
      doi: n.doi,
      x: width / 2 + Math.cos(a) * spread,
      y: height / 2 + Math.sin(a) * spread,
      vx: 0,
      vy: 0,
      r: 6 + Math.min(10, n.cited * 2),
    };
  });
  const index = new Map(points.map((p, i) => [p.key, i]));
  const links: Link[] = [];
  for (const e of graph.edges) {
    const a = index.get(e.a);
    const b = index.get(e.b);
    if (a !== undefined && b !== undefined) links.push({ a, b, weight: e.weight, candidate: false });
  }
  candidates.forEach((c, i) => {
    const a = seededAngle(i, candidates.length) + 0.4;
    points.push({
      key: `candidate:${c.openalex}`,
      sections: [],
      label: c.title.length > 28 ? `${c.title.slice(0, 27)}…` : c.title,
      title: c.title,
      detail: `${c.year ?? 'n.d.'} · cited by ${c.shared} of your references`,
      cited: c.shared,
      issues: [],
      candidate: true,
      doi: c.doi,
      x: width / 2 + Math.cos(a) * (Math.min(width, height) / 2.2),
      y: height / 2 + Math.sin(a) * (Math.min(width, height) / 2.2),
      vx: 0,
      vy: 0,
      r: 5,
    });
    const from = points.length - 1;
    for (const key of c.via) {
      const to = index.get(key);
      if (to !== undefined) links.push({ a: from, b: to, weight: 1, candidate: true });
    }
  });
  return { points, links };
}

/** A few hundred ticks of repulsion, springs and centring. Enough for a bibliography. */
function settle(points: Point[], links: Link[], width: number, height: number) {
  const centreX = width / 2;
  const centreY = height / 2;
  for (let tick = 0; tick < TICKS; tick++) {
    const cooling = 1 - tick / TICKS;
    for (let i = 0; i < points.length; i++) {
      for (let j = i + 1; j < points.length; j++) {
        const p = points[i]!;
        const q = points[j]!;
        let dx = q.x - p.x;
        let dy = q.y - p.y;
        let d2 = dx * dx + dy * dy;
        if (d2 < 1) {
          dx = (i - j) * 0.5 + 0.1;
          dy = (j - i) * 0.5 + 0.1;
          d2 = dx * dx + dy * dy;
        }
        const force = 2600 / d2;
        const d = Math.sqrt(d2);
        const fx = (dx / d) * force;
        const fy = (dy / d) * force;
        p.vx -= fx;
        p.vy -= fy;
        q.vx += fx;
        q.vy += fy;
      }
    }
    for (const l of links) {
      const p = points[l.a]!;
      const q = points[l.b]!;
      const dx = q.x - p.x;
      const dy = q.y - p.y;
      const d = Math.max(1, Math.hypot(dx, dy));
      const rest = l.candidate ? 150 : 90 - Math.min(40, l.weight * 12);
      const k = (l.candidate ? 0.004 : 0.02) * (d - rest);
      const fx = (dx / d) * k * d * 0.1;
      const fy = (dy / d) * k * d * 0.1;
      p.vx += fx;
      p.vy += fy;
      q.vx -= fx;
      q.vy -= fy;
    }
    for (const p of points) {
      p.vx += (centreX - p.x) * 0.002;
      p.vy += (centreY - p.y) * 0.002;
      p.x += Math.max(-12, Math.min(12, p.vx * cooling));
      p.y += Math.max(-12, Math.min(12, p.vy * cooling));
      p.vx *= 0.82;
      p.vy *= 0.82;
      p.x = Math.max(p.r + 8, Math.min(width - p.r - 8, p.x));
      p.y = Math.max(p.r + 8, Math.min(height - p.r - 8, p.y));
    }
  }
}

/// Entries nothing cites belong to no cluster. Park them in a labelled column, so the simulation
/// does not push them into a corner.
function park(points: Point[], width: number, height: number): Point[] {
  const loose = points.filter((p) => !p.candidate && p.cited === 0);
  if (loose.length === 0) return points;
  const x = width - 74;
  const step = Math.min(38, (height - 120) / Math.max(1, loose.length));
  loose.forEach((p, i) => {
    p.x = x;
    p.y = 96 + i * step;
  });
  return points.filter((p) => p.candidate || p.cited > 0);
}

/// Spread the settled layout across the canvas, keeping its shape and leaving room for labels.
function fit(points: Point[], width: number, height: number) {
  if (points.length < 2) {
    for (const p of points) {
      p.x = width / 2;
      p.y = height / 2;
    }
    return;
  }
  const margin = 48;
  const xs = points.map((p) => p.x);
  const ys = points.map((p) => p.y);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const minY = Math.min(...ys);
  const maxY = Math.max(...ys);
  const scale = Math.min((width - margin * 2) / Math.max(1, maxX - minX), (height - margin * 2) / Math.max(1, maxY - minY));
  const offsetX = (width - (maxX - minX) * scale) / 2 - minX * scale;
  const offsetY = (height - (maxY - minY) * scale) / 2 - minY * scale;
  for (const p of points) {
    p.x = p.x * scale + offsetX;
    p.y = p.y * scale + offsetY;
  }
}

function css(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

export function CitationGraph({ id }: { id: string }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [graph, setGraph] = useState<Graph | null>(null);
  const [candidates, setCandidates] = useState<Candidate[]>([]);
  const [looking, setLooking] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  // The hovered node is remembered by key. A re-layout from a section filter or new candidates
  // builds fresh points, and an object reference would go stale and dim the whole graph.
  const [hoverKey, setHoverKey] = useState<string | null>(null);
  const [section, setSection] = useState<string>('');
  const layout = useRef<{ points: Point[]; links: Link[] }>({ points: [], links: [] });

  useEffect(() => {
    let cancelled = false;
    api
      .citations(id)
      .then((g) => !cancelled && setGraph(g))
      .catch((e) => !cancelled && setNote(e instanceof ApiError ? e.message : 'Could not read the bibliography.'));
    const close = (e: KeyboardEvent) => e.key === 'Escape' && (graphOpen.value = false);
    window.addEventListener('keydown', close);
    return () => {
      cancelled = true;
      window.removeEventListener('keydown', close);
    };
  }, [id]);

  // Lay the graph out once per data change, then draw it.
  useEffect(() => {
    const el = canvas.current;
    if (!el || !graph) return;
    const width = el.clientWidth;
    const height = el.clientHeight;
    const ratio = window.devicePixelRatio || 1;
    el.width = width * ratio;
    el.height = height * ratio;
    const ctx = el.getContext('2d');
    if (!ctx) return;
    ctx.scale(ratio, ratio);

    layout.current = build(graph, candidates, width, height);
    settle(layout.current.points, layout.current.links, width, height);
    fit(park(layout.current.points, width, height), width - 150, height);
    draw(ctx, width, height, layout.current, hoverKey, section);
  }, [graph, candidates, section]);

  // Redraw on hover without re-laying out.
  useEffect(() => {
    const el = canvas.current;
    const ctx = el?.getContext('2d');
    if (!el || !ctx || !graph) return;
    draw(ctx, el.clientWidth, el.clientHeight, layout.current, hoverKey, section);
  }, [hoverKey, graph, section]);

  const at = (e: MouseEvent): Point | null => {
    const el = canvas.current;
    if (!el) return null;
    const box = el.getBoundingClientRect();
    const x = e.clientX - box.left;
    const y = e.clientY - box.top;
    let best: Point | null = null;
    let bestD = 18;
    for (const p of layout.current.points) {
      const d = Math.hypot(p.x - x, p.y - y);
      if (d < Math.max(bestD, p.r + 6)) {
        best = p;
        bestD = d;
      }
    }
    return best;
  };

  const findMissing = async () => {
    setLooking(true);
    setNote(null);
    try {
      const r = await api.citationCandidates(id);
      setCandidates(r.candidates);
      setNote(
        r.candidates.length
          ? `${r.candidates.length} candidate${r.candidates.length === 1 ? '' : 's'} from ${r.from} reference list${r.from === 1 ? '' : 's'}. Each is a candidate, not a recommendation.`
          : r.from === 0
            ? 'No reference lists were available — OpenAlex often has none for preprints.'
            : 'Nothing is cited by more than one of your references.',
      );
    } catch (e) {
      setNote(e instanceof ApiError ? e.message : 'The lookup failed.');
    } finally {
      setLooking(false);
    }
  };

  const hover = hoverKey ? (layout.current.points.find((p) => p.key === hoverKey) ?? null) : null;
  const nodes = graph?.nodes.length ?? 0;
  const sections = [...new Set((graph?.nodes ?? []).flatMap((n) => n.sections))].sort((a, b) =>
    a.localeCompare(b, undefined, { numeric: true }),
  );
  return (
    <div class="hv-overlay" onMouseDown={(e) => e.target === e.currentTarget && (graphOpen.value = false)}>
      <div class="hv graph" role="dialog" aria-label="Citation graph">
      <div class="hv-head">
        <b>Citations</b>
        <span class="hv-note">{nodes} entries · lines join works you cite in the same section</span>
        <span style={{ marginLeft: 'auto', display: 'flex', gap: 6, alignItems: 'center' }}>
          {sections.length > 1 && (
            <select class="sel" aria-label="Highlight a section" value={section} onChange={(e) => setSection((e.target as HTMLSelectElement).value)}>
              <option value="">Every section</option>
              {sections.map((s) => (
                <option key={s} value={s}>
                  §{s}
                </option>
              ))}
            </select>
          )}
          {graph?.literature && (
            <button class="tb" disabled={looking} onClick={() => void findMissing()}>
              {looking ? 'Looking…' : 'Find work you may be missing'}
            </button>
          )}
          <button class="tb" onClick={() => (graphOpen.value = false)}>
            Close
          </button>
        </span>
      </div>
      <div class="graph-body">
        <canvas
          ref={canvas}
          onMouseMove={(e) => setHoverKey(at(e as unknown as MouseEvent)?.key ?? null)}
          onMouseLeave={() => setHoverKey(null)}
          onClick={(e) => {
            const p = at(e as unknown as MouseEvent);
            if (!p) return;
            if (p.candidate) {
              const doi = p.doi;
              if (doi) window.open(`https://doi.org/${doi}`, '_blank', 'noopener');
              else showToast(p.title);
              return;
            }
            const entry = p.entry;
            if (!entry) return;
            graphOpen.value = false;
            const hit = requestGoto(entry.file, entry.line);
            if (hit) goToLine(hit.view, hit.line);
          }}
        />
        {hover && (
          <div
            class="gtip"
            style={{
              left: `${Math.max(8, Math.min(hover.x + 14, (canvas.current?.clientWidth ?? 800) - 300))}px`,
              top: `${Math.max(8, Math.min(hover.y + 14, (canvas.current?.clientHeight ?? 600) - 90))}px`,
            }}
          >
            <b>{hover.title}</b>
            {hover.detail && <span>{hover.detail}</span>}
            {!hover.candidate && (
              <span>
                {hover.cited > 0 ? `cited in ${hover.cited} section${hover.cited === 1 ? '' : 's'}` : 'never cited'}
                {hover.issues.length ? ` · ${hover.issues.map((i) => i.replace('bib-', '').replace('-entry', '')).join(', ')}` : ''}
              </span>
            )}
            {hover.candidate && <span>click to open the DOI</span>}
          </div>
        )}
      </div>
      <div class="glegend">
        <span><i class="dot solid" /> cited, bigger when cited in more sections</span>
        <span><i class="dot hollow" /> in the bibliography, never cited</span>
        <span><i class="dot dashed" /> a candidate from the catalogues</span>
        <span><i class="bar" /> cited in the same section</span>
      </div>
      {note && <div class="hint">{note}</div>}
      {!note && project.value && !graph?.literature && (
        <div class="hint">
          Catalogue lookups are off, so this is your bibliography alone. Turning them on in the Bibliography panel adds
          work your references cite and you do not.
        </div>
      )}
      </div>
    </div>
  );
}

function draw(
  ctx: CanvasRenderingContext2D,
  width: number,
  height: number,
  layout: { points: Point[]; links: Link[] },
  hoverKey: string | null,
  section: string,
) {
  const line = css('--line', '#d9d3c6');
  const text = css('--text', '#1b1a17');
  const text2 = css('--text-2', '#5b6470');
  const accent = css('--red', '#c6402a');
  const moss = css('--moss', '#3e7c59');
  const panel = css('--bg-panel', '#fbf9f4');
  ctx.clearRect(0, 0, width, height);

  // What the reader is asking about: a hovered node and its neighbours, or a section's citations.
  const near = new Set<number>();
  const self = hoverKey ? layout.points.findIndex((p) => p.key === hoverKey) : -1;
  const hover = self >= 0 ? layout.points[self]! : null;
  if (hover) {
    near.add(self);
    for (const l of layout.links) {
      if (l.a === self) near.add(l.b);
      if (l.b === self) near.add(l.a);
    }
  }
  const inSection = (p: Point) => !section || p.sections.includes(section);
  const lit = (i: number) => (hover ? near.has(i) : inSection(layout.points[i]!));

  for (const l of layout.links) {
    const p = layout.points[l.a]!;
    const q = layout.points[l.b]!;
    // While hovering, only this node's own edges are drawn strongly.
    const on = hover ? l.a === self || l.b === self : lit(l.a) && lit(l.b);
    ctx.beginPath();
    ctx.moveTo(p.x, p.y);
    ctx.lineTo(q.x, q.y);
    ctx.strokeStyle = on && hover ? accent : line;
    ctx.globalAlpha = on ? (l.candidate ? 0.6 : Math.min(1, 0.4 + l.weight * 0.2)) : 0.08;
    ctx.lineWidth = l.candidate ? 1 : Math.min(3, 1 + l.weight * 0.5);
    ctx.setLineDash(l.candidate ? [4, 4] : []);
    ctx.stroke();
  }
  ctx.setLineDash([]);
  ctx.globalAlpha = 1;

  // Labels claim space. A label that would land on one already drawn is dropped.
  const taken: { x: number; y: number; w: number }[] = [];
  const roomy = layout.points.length <= 24;
  for (let i = 0; i < layout.points.length; i++) {
    const p = layout.points[i]!;
    const on = lit(i);
    const isHover = hover === p;
    ctx.globalAlpha = on ? 1 : 0.25;
    ctx.beginPath();
    ctx.arc(p.x, p.y, p.r, 0, Math.PI * 2);
    ctx.fillStyle = p.candidate || p.cited === 0 || p.issues.length ? panel : accent;
    ctx.fill();
    ctx.lineWidth = isHover ? 3 : 1.5;
    ctx.strokeStyle = p.candidate ? moss : p.issues.length || p.cited === 0 ? line : accent;
    ctx.setLineDash(p.candidate ? [3, 3] : []);
    ctx.stroke();
    ctx.setLineDash([]);

    if (isHover || p.candidate || roomy || p.cited > 1) {
      ctx.font = `${isHover ? '600 ' : ''}11px ui-sans-serif, system-ui, sans-serif`;
      const w = ctx.measureText(p.label).width;
      const x = p.x;
      const y = p.y + p.r + 12;
      const clash = taken.some((t) => Math.abs(t.y - y) < 11 && Math.abs(t.x - x) < (t.w + w) / 2 + 6);
      if (!clash || isHover) {
        ctx.fillStyle = isHover ? text : text2;
        ctx.textAlign = 'center';
        ctx.fillText(p.label, x, y);
        taken.push({ x, y, w });
      }
    }
  }
  ctx.globalAlpha = 1;
}
