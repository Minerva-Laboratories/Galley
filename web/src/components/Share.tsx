import { useEffect, useState } from 'preact/hooks';
import {
  api,
  ApiError,
  type DeviceToken,
  type GovernanceMode,
  type GovernResult,
  type Member,
  type Role2,
  type RoleRequest,
  type ShareLink,
} from '../api';
import { currentUser, project, shareOpen, sharingNonce, showToast } from '../store/store';
import { initials } from '../sync/colors';

const ROLES: Role2[] = ['admin', 'editor', 'commenter', 'viewer'];
const LINK_ROLES: Role2[] = ['editor', 'commenter', 'viewer'];

const MODE_LABEL: Record<GovernanceMode, string> = {
  solo: 'Any admin can change roles alone',
  majority: 'A majority of admins must approve',
  unanimous: 'Every admin must approve',
};

function outcomeToast(r: GovernResult, appliedMsg: string) {
  if (r.status === 'applied') showToast(appliedMsg);
  else if (r.status === 'pending') showToast('Requested — it needs other admins to approve.');
  else if (r.status === 'rejected') showToast('That change was rejected.');
}

/** The one-liner that registers this project in Claude Code. Other clients take the same URL and header. */
function mcpAddCommand(projectId: string, token: string): string {
  return `claude mcp add --transport http galley ${api.mcpUrl(projectId)} --header "Authorization: Bearer ${token}"`;
}

/** The same project driven by a local model with no client. The `galley` binary is the runner. */
function runnerCommand(projectId: string, token: string): string {
  return `galley agent run --url ${api.mcpUrl(projectId)} --token ${token} --model qwen2.5-coder:14b`;
}

export function ShareModal() {
  const id = project.value?.id;
  const [members, setMembers] = useState<Member[]>([]);
  const [links, setLinks] = useState<ShareLink[]>([]);
  const [mode, setMode] = useState<GovernanceMode>('solo');
  const [requests, setRequests] = useState<RoleRequest[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [email, setEmail] = useState('');
  const [inviteRole, setInviteRole] = useState<Role2>('editor');
  const [linkRole, setLinkRole] = useState<Role2>('commenter');
  const [linkExpiry, setLinkExpiry] = useState(30);
  const [madeLink, setMadeLink] = useState<string | null>(null);
  const [tokens, setTokens] = useState<DeviceToken[]>([]);
  const [tokenLabel, setTokenLabel] = useState('Claude Code');
  const [madeToken, setMadeToken] = useState<string | null>(null);
  const nonce = sharingNonce.value;

  const refresh = async () => {
    if (!id) return;
    try {
      const [m, l, g, t] = await Promise.all([api.members(id), api.shares(id), api.governance(id), api.tokens().catch(() => [])]);
      setMembers(m);
      setLinks(l);
      setTokens(t);
      setMode(g.mode);
      setRequests(g.requests);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not load sharing.');
    }
  };
  useEffect(() => {
    void refresh();
  }, [id, nonce]);

  const close = () => (shareOpen.value = false);

  const copyText = (text: string) => {
    void navigator.clipboard.writeText(text).then(
      () => showToast('Copied'),
      () => showToast('Could not copy. Select the text and copy it by hand.'),
    );
  };
  const createToken = async () => {
    try {
      const r = await api.createToken(tokenLabel.trim());
      setMadeToken(r.token);
      setTokens((prev) => [{ id: r.id, label: r.label, created_at: new Date().toISOString(), last_used_at: null }, ...prev]);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not create the token.');
    }
  };
  const revokeToken = async (t: DeviceToken) => {
    try {
      await api.revokeToken(t.id);
      setTokens((prev) => prev.filter((x) => x.id !== t.id));
      if (madeToken) setMadeToken(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not revoke the token.');
    }
  };
  const adminCount = members.filter((m) => m.role === 'admin' && !m.is_guest).length;

  const invite = async (e: Event) => {
    e.preventDefault();
    if (!id) return;
    try {
      const r = await api.invite(id, email.trim(), inviteRole);
      outcomeToast(r, 'Added.');
      setEmail('');
      setError(null);
      void refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'Could not invite.');
    }
  };

  const changeRole = async (m: Member, role: Role2) => {
    if (!id) return;
    try {
      const r = await api.setRole(id, m.user_id, role);
      outcomeToast(r, `${m.name} is now ${role}.`);
      void refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'Could not change the role.');
    }
  };

  const remove = async (m: Member) => {
    if (!id) return;
    try {
      const r = await api.removeMember(id, m.user_id);
      outcomeToast(r, `Removed ${m.name}.`);
      void refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'Could not remove the member.');
    }
  };

  const changeMode = async (next: GovernanceMode) => {
    if (!id || next === mode) return;
    try {
      const r = await api.setGovernance(id, next);
      outcomeToast(r, `Role changes now: ${MODE_LABEL[next].toLowerCase()}.`);
      void refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'Could not change governance.');
    }
  };

  const createLink = async () => {
    if (!id) return;
    try {
      const link = await api.createShare(id, linkRole, null, linkExpiry);
      setMadeLink(`${location.origin}${link.path}`);
      void refresh();
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'Could not create a link.');
    }
  };

  const revoke = async (link: ShareLink) => {
    if (!id) return;
    try {
      await api.revokeShare(id, link.id);
      setLinks((prev) => prev.filter((x) => x.id !== link.id));
    } catch (err) {
      setError(err instanceof ApiError ? err.message : 'Could not revoke the link.');
    }
  };

  const copy = (text: string) =>
    navigator.clipboard?.writeText(text).then(
      () => showToast('Link copied'),
      () => showToast('Copy failed; select and copy it by hand.'),
    );

  return (
    <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div class="modal" role="dialog" aria-label="Share project">
        <div class="mh">
          Share “{project.value?.name}”
          <button class="tb" style={{ marginLeft: 'auto' }} onClick={close}>
            Done
          </button>
        </div>
        <div class="mb">
          {error && <div class="err">{error}</div>}

          {requests.length > 0 && (
            <>
              <div style={{ fontWeight: 500, marginBottom: 4 }}>Pending changes</div>
              {requests.map((r) => (
                <RequestCard key={r.id} req={r} projectId={id!} adminCount={adminCount} onDone={refresh} />
              ))}
            </>
          )}

          {members.map((m) => (
            <div class="member" key={m.user_id}>
              <span class="av" style={{ background: 'var(--slate)' }}>{initials(m.name)}</span>
              <div style={{ minWidth: 0 }}>
                <div class="n">{m.name}{m.is_guest ? ' · guest' : ''}</div>
                <div class="e">{m.email || 'joined by link'}</div>
              </div>
              {m.user_id === currentUser.value?.id ? (
                <span style={{ marginLeft: 'auto', fontSize: 12, color: 'var(--text-2)' }}>{m.role} (you)</span>
              ) : (
                <>
                  <select value={m.role} onChange={(e) => void changeRole(m, (e.target as HTMLSelectElement).value as Role2)} style={{ marginLeft: 'auto' }}>
                    {ROLES.map((r) => (
                      <option key={r} value={r}>
                        {r}
                      </option>
                    ))}
                  </select>
                  <button class="tb icon" title="Remove" aria-label={`Remove ${m.name}`} onClick={() => void remove(m)}>
                    ×
                  </button>
                </>
              )}
            </div>
          ))}

          <form class="field" style={{ marginTop: 14 }} onSubmit={invite}>
            <input placeholder="name@university.edu" type="email" value={email} onInput={(e) => setEmail((e.target as HTMLInputElement).value)} />
            <select value={inviteRole} onChange={(e) => setInviteRole((e.target as HTMLSelectElement).value as Role2)}>
              {ROLES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
            <button class="tb" type="submit" disabled={!email.trim()}>
              Invite
            </button>
          </form>
          <div class="hint" style={{ padding: '0 0 4px' }}>People must sign in once before you can add them by email.</div>

          <div style={{ marginTop: 16, fontWeight: 500 }}>Who can change roles</div>
          <div class="field">
            <select value={mode} onChange={(e) => void changeMode((e.target as HTMLSelectElement).value as GovernanceMode)} style={{ flex: 1 }}>
              <option value="solo">Any admin (solo)</option>
              <option value="majority">Majority of admins</option>
              <option value="unanimous">All admins (unanimous)</option>
            </select>
          </div>
          <div class="hint" style={{ paddingLeft: 0 }}>
            {MODE_LABEL[mode]}. {mode !== 'solo' ? 'Changes wait as pending requests other admins vote on.' : ''}
            {adminCount > 1 ? ` ${adminCount} admins.` : ''}
          </div>

          <div style={{ marginTop: 16, fontWeight: 500 }}>Share link</div>
          <div class="field">
            <select value={linkRole} onChange={(e) => setLinkRole((e.target as HTMLSelectElement).value as Role2)}>
              {LINK_ROLES.map((r) => (
                <option key={r} value={r}>
                  {r === 'editor' ? 'Can edit' : r === 'commenter' ? 'Can comment' : 'Can view'}
                </option>
              ))}
            </select>
            <select value={String(linkExpiry)} onChange={(e) => setLinkExpiry(Number((e.target as HTMLSelectElement).value))}>
              <option value="30">Expires in 30 days</option>
              <option value="7">Expires in 7 days</option>
              <option value="0">Never expires</option>
            </select>
            <button class="tb" onClick={() => void createLink()}>
              Create link
            </button>
          </div>
          {madeLink && (
            <div class="link">
              <span>{madeLink}</span>
              <button class="tb" onClick={() => copy(madeLink)}>
                Copy
              </button>
            </div>
          )}
          {links.map((l) => (
            <div class="link" key={l.id}>
              <span>
                {l.role} · {l.expires_at ? `expires ${new Date(l.expires_at).toLocaleDateString()}` : 'never expires'}
              </span>
              <button class="tb" onClick={() => void revoke(l)}>
                Revoke
              </button>
            </div>
          ))}
          <div class="hint" style={{ paddingLeft: 0 }}>Reviewers who open a link only need a display name.</div>

          <div style={{ marginTop: 16, fontWeight: 500 }}>AI clients</div>
          <div class="hint" style={{ paddingLeft: 0 }}>
            Connect Claude Code, Cursor or any MCP client, or run a local model with the galley binary alone. Either way it
            runs on your machine with your model; its edits arrive here as suggestions marked “via” the agent, for the
            authors to accept or reject.
          </div>
          <div class="link">
            <span>{api.mcpUrl(id!)}</span>
            <button class="tb" onClick={() => copyText(api.mcpUrl(id!))}>
              Copy URL
            </button>
          </div>
          <div class="field">
            <input placeholder="Client name" value={tokenLabel} onInput={(e) => setTokenLabel((e.target as HTMLInputElement).value)} />
            <button class="tb" onClick={() => void createToken()} disabled={!tokenLabel.trim()}>
              Create token
            </button>
          </div>
          {madeToken && (
            <>
              <div class="link">
                <span>{madeToken}</span>
                <button class="tb" onClick={() => copyText(madeToken)}>
                  Copy token
                </button>
              </div>
              <div class="link">
                <span style={{ fontFamily: 'var(--mono)', fontSize: 11 }}>{mcpAddCommand(id!, madeToken)}</span>
                <button class="tb" onClick={() => copyText(mcpAddCommand(id!, madeToken))}>
                  Copy command
                </button>
              </div>
              <div class="link">
                <span style={{ fontFamily: 'var(--mono)', fontSize: 11 }}>{runnerCommand(id!, madeToken)}</span>
                <button class="tb" onClick={() => copyText(runnerCommand(id!, madeToken))}>
                  Copy command
                </button>
              </div>
              <div class="hint" style={{ paddingLeft: 0 }}>
                This token is shown once. It works on every project you can open. The second command needs Ollama (or any
                OpenAI-compatible endpoint) and a tool-capable model of about 7B or more; smaller ones tend to stop early.
              </div>
            </>
          )}
          {tokens.map((t) => (
            <div class="link" key={t.id}>
              <span>
                {t.label} · {t.last_used_at ? `used ${new Date(t.last_used_at).toLocaleDateString()}` : 'never used'}
              </span>
              <button class="tb" onClick={() => void revokeToken(t)}>
                Revoke
              </button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

function RequestCard({ req, projectId, adminCount, onDone }: { req: RoleRequest; projectId: string; adminCount: number; onDone: () => Promise<void> }) {
  const approvals = req.votes.filter((v) => v.vote === 'approve').length;
  const rejects = req.votes.filter((v) => v.vote === 'reject').length;
  const mine = req.votes.find((v) => v.admin_id === currentUser.value?.id);
  const isProposer = req.proposer_id === currentUser.value?.id;

  const vote = async (approve: boolean) => {
    try {
      const r = await api.voteRequest(projectId, req.id, approve);
      if (r.status === 'applied') showToast('Approved — the change was applied.');
      else if (r.status === 'rejected') showToast('The request was rejected.');
      await onDone();
    } catch (e) {
      showToast(e instanceof ApiError ? e.message : 'Could not record your vote.');
    }
  };
  const cancel = async () => {
    try {
      await api.cancelRequest(projectId, req.id);
      await onDone();
    } catch (e) {
      showToast(e instanceof ApiError ? e.message : 'Could not cancel.');
    }
  };

  return (
    <div class="card" style={{ marginBottom: 8 }}>
      <div class="t" style={{ fontWeight: 500 }}>{req.summary}</div>
      <div class="loc" style={{ marginTop: 3 }}>
        proposed by {req.proposer_name} · {approvals} of {adminCount} approved{rejects ? ` · ${rejects} rejected` : ''}
      </div>
      <div class="acts">
        {!mine || mine.vote !== 'approve' ? (
          <button class="tb" onClick={() => void vote(true)}>
            Approve
          </button>
        ) : null}
        {!mine || mine.vote !== 'reject' ? (
          <button class="tb" onClick={() => void vote(false)}>
            Reject
          </button>
        ) : null}
        {isProposer && (
          <button class="tb" onClick={() => void cancel()}>
            Cancel
          </button>
        )}
      </div>
    </div>
  );
}
