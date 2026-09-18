// Typed wrappers over the Galley REST API. Every failure becomes an ApiError whose message is
// already written for people (the server composes them in sentence case with a next step).
import type { Venue } from './editor/compliance';

export interface Template {
  id: string;
  name: string;
  description: string;
  main_file: string;
  venue: string | null;
}

export interface ProjectMeta {
  id: string;
  name: string;
  main_file: string;
  created_at: string;
  updated_at: string;
  lint_disabled?: string[];
  /** YYYY-MM-DD */
  deadline?: string | null;
  venue?: string | null;
  /** Word budgets by section title. */
  budgets?: Record<string, number>;
  /** Commit of the "Submitted to …" checkpoint. */
  submitted_checkpoint?: string | null;
  figure_cache?: boolean;
}

/** How the build used the persistent figure cache (SPEC §13.1). */
export interface FigureStats {
  mode: 'cached' | 'plain' | 'off';
  total: number;
  cached: number;
  pending: number;
  reason?: string;
}

export type { Venue } from './editor/compliance';

export interface PackItem {
  path: string;
  why: string;
}

export interface PackReport {
  included: PackItem[];
  left_out: PackItem[];
  build_ok: boolean;
  errors: Diagnostic[];
  pages: number | null;
  archive: boolean;
}

/** One bibliography entry as the graph sees it. */
export interface CitationNode {
  key: string;
  title: string | null;
  year: string | null;
  venue: string | null;
  doi: string | null;
  /** How many sections cite it. */
  cited: number;
  sections: string[];
  /** Lint codes from the bibliography audit. */
  issues: string[];
  file: string;
  line: number;
}

/** Two entries cited together, in `weight` sections. */
export interface CitationEdge {
  a: string;
  b: string;
  weight: number;
}

/** One bibliography entry, with every field the file holds. */
export interface BibEntry {
  key: string;
  kind: string;
  fields: [string, string][];
  file: string;
  line: number;
  cited: number;
  sections: string[];
  issues: string[];
  duplicate_of?: string | null;
}

export interface Library {
  entries: BibEntry[];
  files: string[];
  missing: string[];
  literature: boolean;
}

export interface CitationGraph {
  nodes: CitationNode[];
  edges: CitationEdge[];
  /** Cited keys with no entry. */
  missing: string[];
  /** Whether catalogue lookups are on for this project. */
  literature: boolean;
}

/** A work several of your references cite and you do not. */
export interface Candidate {
  title: string;
  year: number | null;
  doi: string | null;
  openalex: string;
  shared: number;
  via: string[];
}

export interface SettingsPatch {
  deadline?: string;
  venue?: string;
  budgets?: Record<string, number>;
  lint_disabled?: string[];
  figure_cache?: boolean;
  main_file?: string;
  literature?: boolean;
}

export interface FileEntry {
  path: string;
  kind: 'text' | 'binary';
  size: number;
}

export interface CommitInfo {
  sha: string;
  short_sha: string;
  message: string;
  author: string;
  time: string;
}

export interface Checkpoint {
  id: string;
  commit_sha: string;
  short_sha: string;
  label: string;
  author_name: string | null;
  created_at: string;
}

export interface DiffLine {
  origin: string; // ' ' context, '+' added, '-' removed
  old_lineno?: number;
  new_lineno?: number;
  content: string;
}

export interface Hunk {
  header: string;
  lines: DiffLine[];
}

export interface FileDiff {
  path: string;
  status: 'added' | 'deleted' | 'modified' | 'renamed' | 'copied';
  old_path?: string;
  binary: boolean;
  hunks: Hunk[];
}

/** A commit sha, or the sentinel meaning the live working tree. */
export const WORKDIR = 'WORKDIR';

export interface DeviceToken {
  id: string;
  label: string;
  created_at: string;
  last_used_at: string | null;
}

export interface AgentRun {
  id: string;
  agent: string;
  model: string | null;
  summary: string;
  edits: number;
  author_name: string;
  created_at: string;
}

export type Level = 'error' | 'warning' | 'info' | 'lint';

export type Fix =
  | { kind: 'insert'; label: string; file: string; after_line: number; text: string }
  | { kind: 'replace'; label: string; file: string; line: number; find: string; text: string };

export interface Diagnostic {
  level: Level;
  file: string | null;
  line: number | null;
  code: string;
  message: string;
  hint?: string;
  explain?: string;
  fix?: Fix;
  raw: string;
}

export type BuildStatus = 'ok' | 'failed' | 'timeout' | 'error';

export interface BuildResult {
  id: number;
  status: BuildStatus;
  errors: Diagnostic[];
  error_count: number;
  warning_count: number;
  profile: { total_ms: number; compile_ms: number; fetch_ms: number; figure_ms?: number };
  pages?: number | null;
  figures?: FigureStats;
  fetched_packages: boolean;
  pdf_fresh: boolean;
  pdf_available: boolean;
  stale: boolean;
  draft: boolean;
  main_file: string;
  engine: string;
  sandbox: string;
  finished_at: string;
  message?: string;
}

export interface PdfLocation {
  page: number;
  x: number;
  y: number;
  height: number;
}

export interface SourceLocation {
  file: string;
  line: number;
}

export type ProjectEvent =
  | { type: 'commit'; sha: string; short_sha: string; message: string; author: string; time: string }
  | { type: 'file_created'; path: string }
  | { type: 'file_deleted'; path: string }
  | { type: 'file_renamed'; from: string; to: string }
  | { type: 'files_changed' }
  | { type: 'build_started'; id: number; draft: boolean }
  | { type: 'build_progress'; id: number; message: string }
  | ({ type: 'build_finished' } & BuildResult)
  | CollabEvent;

export interface AuthUser {
  id: string;
  name: string;
  email: string;
  is_admin: boolean;
  is_guest: boolean;
}

export interface Member {
  user_id: string;
  name: string;
  email: string;
  role: Role2;
  is_guest: boolean;
}

export type Role2 = 'admin' | 'editor' | 'commenter' | 'viewer';

export type GovernanceMode = 'solo' | 'majority' | 'unanimous';

export interface VoteRecord {
  admin_id: string;
  admin_name: string;
  vote: 'approve' | 'reject';
}

export interface RoleRequest {
  id: string;
  kind: 'set_role' | 'remove_member' | 'invite' | 'set_governance';
  target_id: string | null;
  payload: Record<string, unknown>;
  summary: string;
  proposer_id: string;
  proposer_name: string;
  status: string;
  created_at: string;
  votes: VoteRecord[];
}

export interface GovernanceState {
  mode: GovernanceMode;
  requests: RoleRequest[];
}

/** What a governed action returned: applied now, or waiting for votes. */
export interface GovernResult {
  status: 'applied' | 'pending' | 'rejected' | 'gone';
  request_id?: string;
}

export interface ShareLink {
  id: string;
  project_id: string;
  role: Role2;
  label: string | null;
  expires_at: string | null;
  created_at: string;
  revoked: boolean;
}

export interface Comment {
  id: string;
  file: string;
  anchor: string;
  quote: string | null;
  author_id: string;
  author_name: string;
  body: string;
  resolved: boolean;
  created_at: string;
}

export interface Suggestion {
  id: string;
  file: string;
  anchor: string;
  anchor_end: string;
  quote: string;
  replacement: string;
  author_id: string;
  author_name: string;
  status: 'open' | 'accepted' | 'rejected';
  created_at: string;
}

export type CollabEvent =
  | { type: 'comment_added'; comment: Comment }
  | { type: 'comment_resolved'; id: string; resolved: boolean }
  | { type: 'suggestion_added'; suggestion: Suggestion }
  | { type: 'suggestion_updated'; id: string; status: 'accepted' | 'rejected' }
  | { type: 'members_changed' }
  | { type: 'requests_changed' }
  | { type: 'checkpoints_changed' };

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

/** The CSRF token the server set as a readable cookie. It is echoed on state-changing requests. */
function csrfToken(): string | null {
  const m = /(?:^|;\s*)galley_csrf=([^;]+)/.exec(document.cookie);
  return m ? decodeURIComponent(m[1]!) : null;
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const method = (init?.method ?? 'GET').toUpperCase();
  const headers: Record<string, string> = { 'content-type': 'application/json', ...(init?.headers as Record<string, string>) };
  if (method !== 'GET' && method !== 'HEAD') {
    const csrf = csrfToken();
    if (csrf) headers['x-csrf-token'] = csrf;
  }
  let res: Response;
  try {
    res = await fetch(path, { ...init, headers, credentials: 'same-origin' });
  } catch {
    throw new ApiError(0, 'The server is unreachable. Check that galley serve is running.');
  }
  if (!res.ok) {
    let message = `Request failed (${res.status}).`;
    try {
      const body = (await res.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // A non-JSON error body: keep the generic message.
    }
    throw new ApiError(res.status, message);
  }
  return (await res.json()) as T;
}

export interface Me {
  user: AuthUser | null;
  needs_setup: boolean;
  public_signup: boolean;
}

export const api = {
  me: () => request<Me>('/api/auth/me'),
  signup: (email: string, name: string, password: string) =>
    request<{ user: AuthUser }>('/api/auth/signup', { method: 'POST', body: JSON.stringify({ email, name, password }) }),
  login: (email: string, password: string) =>
    request<{ user: AuthUser }>('/api/auth/login', { method: 'POST', body: JSON.stringify({ email, password }) }),
  logout: () => request<{ ok: boolean }>('/api/auth/logout', { method: 'POST' }),
  sharePreview: (token: string) =>
    request<{ project_name: string; role: Role2 }>(`/api/share/${encodeURIComponent(token)}`),
  landing: (token: string, name: string) =>
    request<{ user: AuthUser; role: Role2 }>('/api/auth/landing', { method: 'POST', body: JSON.stringify({ token, name }) }),

  members: (id: string) => request<Member[]>(`/api/projects/${encodeURIComponent(id)}/members`),
  invite: (id: string, email: string, role: Role2) =>
    request<GovernResult>(`/api/projects/${encodeURIComponent(id)}/members`, { method: 'POST', body: JSON.stringify({ email, role }) }),
  setRole: (id: string, userId: string, role: Role2) =>
    request<GovernResult>(`/api/projects/${encodeURIComponent(id)}/members/${encodeURIComponent(userId)}`, {
      method: 'PUT',
      body: JSON.stringify({ role }),
    }),
  removeMember: (id: string, userId: string) =>
    request<GovernResult>(`/api/projects/${encodeURIComponent(id)}/members/${encodeURIComponent(userId)}`, { method: 'DELETE' }),
  governance: (id: string) => request<GovernanceState>(`/api/projects/${encodeURIComponent(id)}/governance`),
  setGovernance: (id: string, mode: GovernanceMode) =>
    request<GovernResult>(`/api/projects/${encodeURIComponent(id)}/governance`, { method: 'PUT', body: JSON.stringify({ mode }) }),
  voteRequest: (id: string, requestId: string, approve: boolean) =>
    request<{ status: string }>(`/api/projects/${encodeURIComponent(id)}/requests/${encodeURIComponent(requestId)}/vote`, {
      method: 'POST',
      body: JSON.stringify({ approve }),
    }),
  cancelRequest: (id: string, requestId: string) =>
    request<{ ok: boolean }>(`/api/projects/${encodeURIComponent(id)}/requests/${encodeURIComponent(requestId)}`, { method: 'DELETE' }),
  shares: (id: string) => request<ShareLink[]>(`/api/projects/${encodeURIComponent(id)}/shares`),
  createShare: (id: string, role: Role2, label: string | null, expiresDays: number) =>
    request<{ id: string; role: Role2; label: string | null; expires_at: string | null; path: string }>(
      `/api/projects/${encodeURIComponent(id)}/shares`,
      { method: 'POST', body: JSON.stringify({ role, label, expires_days: expiresDays }) },
    ),
  revokeShare: (id: string, shareId: string) =>
    request<{ ok: boolean }>(`/api/projects/${encodeURIComponent(id)}/shares/${encodeURIComponent(shareId)}`, { method: 'DELETE' }),

  comments: (id: string) => request<Comment[]>(`/api/projects/${encodeURIComponent(id)}/comments`),
  addComment: (id: string, file: string, anchor: string, quote: string | null, body: string) =>
    request<Comment>(`/api/projects/${encodeURIComponent(id)}/comments`, {
      method: 'POST',
      body: JSON.stringify({ file, anchor, quote, body }),
    }),
  resolveComment: (id: string, commentId: string, resolved: boolean) =>
    request<{ ok: boolean }>(`/api/projects/${encodeURIComponent(id)}/comments/${encodeURIComponent(commentId)}/resolve`, {
      method: 'POST',
      body: JSON.stringify({ resolved }),
    }),
  suggestions: (id: string) => request<Suggestion[]>(`/api/projects/${encodeURIComponent(id)}/suggestions`),
  addSuggestion: (id: string, s: { file: string; anchor: string; anchor_end: string; quote: string; replacement: string }) =>
    request<Suggestion>(`/api/projects/${encodeURIComponent(id)}/suggestions`, { method: 'POST', body: JSON.stringify(s) }),
  setSuggestionStatus: (id: string, suggestionId: string, status: 'accepted' | 'rejected') =>
    request<{ ok: boolean }>(`/api/projects/${encodeURIComponent(id)}/suggestions/${encodeURIComponent(suggestionId)}/status`, {
      method: 'POST',
      body: JSON.stringify({ status }),
    }),

  listProjects: () => request<(ProjectMeta & { role: Role2 })[]>('/api/projects'),
  listTemplates: () => request<Template[]>('/api/templates'),
  checkGrammar: (id: string, path: string) =>
    request<Diagnostic[]>(`/api/projects/${encodeURIComponent(id)}/grammar`, { method: 'POST', body: JSON.stringify({ path }) }),
  createProject: (name: string, template?: string) =>
    request<ProjectMeta>('/api/projects', { method: 'POST', body: JSON.stringify({ name, template }) }),
  getProject: (id: string) => request<ProjectMeta & { role: Role2 }>(`/api/projects/${encodeURIComponent(id)}`),
  updateSettings: (id: string, patch: SettingsPatch) =>
    request<ProjectMeta & { role: Role2 }>(`/api/projects/${encodeURIComponent(id)}/settings`, {
      method: 'PATCH',
      body: JSON.stringify(patch),
    }),
  listFiles: (id: string) => request<FileEntry[]>(`/api/projects/${encodeURIComponent(id)}/files`),
  createFile: (id: string, path: string) =>
    request<FileEntry>(`/api/projects/${encodeURIComponent(id)}/files`, {
      method: 'POST',
      body: JSON.stringify({ path }),
    }),
  deleteFile: (id: string, path: string) =>
    request<{ ok: boolean }>(`/api/projects/${encodeURIComponent(id)}/files?path=${encodeURIComponent(path)}`, {
      method: 'DELETE',
    }),
  renameFile: (id: string, from: string, to: string) =>
    request<{ path: string }>(`/api/projects/${encodeURIComponent(id)}/files/rename`, {
      method: 'POST',
      body: JSON.stringify({ from, to }),
    }),
  uploadFile: (id: string, path: string, file: Blob, replace = false) =>
    request<FileEntry>(
      `/api/projects/${encodeURIComponent(id)}/files/content?path=${encodeURIComponent(path)}${replace ? '&replace=true' : ''}`,
      { method: 'PUT', body: file, headers: { 'content-type': 'application/octet-stream' } },
    ),
  fileUrl: (id: string, path: string, inline = false) =>
    `/api/projects/${encodeURIComponent(id)}/files/content?path=${encodeURIComponent(path)}${inline ? '&inline=true' : ''}`,
  history: (id: string, limit = 50) =>
    request<CommitInfo[]>(`/api/projects/${encodeURIComponent(id)}/history?limit=${limit}`),
  checkpoints: (id: string) =>
    request<Checkpoint[]>(`/api/projects/${encodeURIComponent(id)}/checkpoints`),
  createCheckpoint: (id: string, label: string) =>
    request<Checkpoint>(`/api/projects/${encodeURIComponent(id)}/checkpoints`, {
      method: 'POST',
      body: JSON.stringify({ label }),
    }),
  fileAt: (id: string, rev: string, path: string) =>
    request<{ content: string | null }>(
      `/api/projects/${encodeURIComponent(id)}/history/file?rev=${encodeURIComponent(rev)}&path=${encodeURIComponent(path)}`,
    ),
  diff: (id: string, from: string, to: string, path?: string) => {
    const q = new URLSearchParams({ from, to });
    if (path) q.set('path', path);
    return request<FileDiff[]>(`/api/projects/${encodeURIComponent(id)}/history/diff?${q.toString()}`);
  },
  restore: (id: string, rev: string, label?: string) =>
    request<{ commit: CommitInfo | null }>(`/api/projects/${encodeURIComponent(id)}/restore`, {
      method: 'POST',
      body: JSON.stringify({ rev, label }),
    }),
  latexdiff: (id: string, from: string, to: string, file?: string) =>
    request<{ pdf_available: boolean; file: string }>(`/api/projects/${encodeURIComponent(id)}/latexdiff`, {
      method: 'POST',
      body: JSON.stringify({ from, to, file }),
    }),
  latexdiffPdfUrl: (id: string) => `/api/projects/${encodeURIComponent(id)}/latexdiff/pdf`,
  tokens: () => request<DeviceToken[]>('/api/auth/tokens'),
  createToken: (label: string) =>
    request<{ token: string; id: string; label: string }>('/api/auth/tokens', {
      method: 'POST',
      body: JSON.stringify({ label }),
    }),
  revokeToken: (id: string) =>
    request<{ ok: boolean }>(`/api/auth/tokens/${encodeURIComponent(id)}`, { method: 'DELETE' }),
  agentRuns: (id: string) => request<AgentRun[]>(`/api/projects/${encodeURIComponent(id)}/agent-runs`),
  /** The MCP endpoint an AI client connects to for this project. */
  mcpUrl: (id: string) => `${location.origin}/mcp/${encodeURIComponent(id)}`,
  flush: (id: string, message?: string) =>
    request<{ commit: CommitInfo | null }>(`/api/projects/${encodeURIComponent(id)}/flush`, {
      body: message ? JSON.stringify({ message }) : undefined,
      method: 'POST',
    }),
  build: (id: string, draft: boolean, file?: string) =>
    request<{ queued: boolean }>(`/api/projects/${encodeURIComponent(id)}/build`, {
      method: 'POST',
      body: JSON.stringify({ draft, file }),
    }),
  buildStatus: (id: string) =>
    request<{ last: BuildResult | null; running: boolean; sandbox: string; engine: string; latexdiff: boolean }>(
      `/api/projects/${encodeURIComponent(id)}/build`,
    ),
  pdfUrl: (id: string, version: number) => `/api/projects/${encodeURIComponent(id)}/build/pdf?v=${version}`,
  library: (id: string) => request<Library>(`/api/projects/${encodeURIComponent(id)}/bib`),
  addEntry: (id: string, body: { identifier?: string; bibtex?: string; file?: string }) =>
    request<{ key: string; file: string; kind: string; fields: [string, string][] }>(
      `/api/projects/${encodeURIComponent(id)}/bib`,
      { method: 'POST', body: JSON.stringify(body) },
    ),
  editEntry: (id: string, key: string, fields: Record<string, string>, kind?: string) =>
    request<{ key: string; fields: [string, string][] }>(
      `/api/projects/${encodeURIComponent(id)}/bib/${encodeURIComponent(key)}`,
      { method: 'PATCH', body: JSON.stringify({ fields, kind }) },
    ),
  deleteEntry: (id: string, key: string, force = false) =>
    request<{ ok: boolean; was_cited: number }>(
      `/api/projects/${encodeURIComponent(id)}/bib/${encodeURIComponent(key)}${force ? '?force=true' : ''}`,
      { method: 'DELETE' },
    ),
  mergeEntry: (id: string, key: string, into: string) =>
    request<{ ok: boolean; moved: number }>(
      `/api/projects/${encodeURIComponent(id)}/bib/${encodeURIComponent(key)}/merge`,
      { method: 'POST', body: JSON.stringify({ into }) },
    ),
  lookupEntry: (id: string, key: string) =>
    request<{ key: string; suggestions: [string, string][] }>(
      `/api/projects/${encodeURIComponent(id)}/bib/${encodeURIComponent(key)}/lookup`,
      { method: 'POST' },
    ),
  citations: (id: string) => request<CitationGraph>(`/api/projects/${encodeURIComponent(id)}/citations`),
  citationCandidates: (id: string) =>
    request<{ candidates: Candidate[]; from: number; notes: string[] }>(
      `/api/projects/${encodeURIComponent(id)}/citations/candidates`,
      { method: 'POST' },
    ),
  venues: () => request<Venue[]>('/api/venues'),
  pack: (id: string) => request<PackReport>(`/api/projects/${encodeURIComponent(id)}/pack`, { method: 'POST' }),
  clearFigures: (id: string) =>
    request<{ removed: number }>(`/api/projects/${encodeURIComponent(id)}/figures/clear`, { method: 'POST' }),
  packDownloadUrl: (id: string) => `/api/projects/${encodeURIComponent(id)}/pack/download`,
  freeze: (id: string, venue?: string) =>
    request<{ checkpoint: Checkpoint }>(`/api/projects/${encodeURIComponent(id)}/submitted`, {
      method: 'POST',
      body: JSON.stringify(venue ? { venue } : {}),
    }),
  logUrl: (id: string) => `/api/projects/${encodeURIComponent(id)}/build/log`,
  synctexForward: (id: string, file: string, line: number) =>
    request<PdfLocation>(
      `/api/projects/${encodeURIComponent(id)}/build/synctex/forward?file=${encodeURIComponent(file)}&line=${line}`,
    ),
  synctexInverse: (id: string, page: number, x: number, y: number) =>
    request<SourceLocation>(
      `/api/projects/${encodeURIComponent(id)}/build/synctex/inverse?page=${page}&x=${x.toFixed(2)}&y=${y.toFixed(2)}`,
    ),
};

export function wsBase(): string {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return `${proto}//${location.host}`;
}
