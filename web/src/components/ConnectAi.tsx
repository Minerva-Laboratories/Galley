import { useEffect, useState } from 'preact/hooks';
import { api, ApiError, type DeviceToken } from '../api';
import { connectOpen, project, showToast } from '../store/store';

function mcpAddCommand(projectId: string, token: string): string {
  return `claude mcp add --transport http galley ${api.mcpUrl(projectId)} --header "Authorization: Bearer ${token}"`;
}

function runnerCommand(projectId: string, token: string): string {
  return `galley agent run --url ${api.mcpUrl(projectId)} --token ${token} --model qwen2.5-coder:14b`;
}

/** Connecting an AI client is a main feature, so it has its own panel rather than a corner of Share.
 * Any signed-in author can connect: a token belongs to the person and reaches every project they can
 * open, with the role they hold there. */
export function ConnectAiModal() {
  const id = project.value?.id;
  const [tokens, setTokens] = useState<DeviceToken[]>([]);
  const [label, setLabel] = useState('Claude Code');
  const [made, setMade] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && (connectOpen.value = false);
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  useEffect(() => {
    api
      .tokens()
      .then(setTokens)
      .catch(() => setTokens([]));
  }, []);

  if (!id) return null;
  const close = () => (connectOpen.value = false);
  const copy = (text: string) =>
    void navigator.clipboard.writeText(text).then(
      () => showToast('Copied'),
      () => showToast('Could not copy. Select the text and copy it by hand.'),
    );

  const create = async () => {
    try {
      const r = await api.createToken(label.trim());
      setMade(r.token);
      setTokens((prev) => [{ id: r.id, label: r.label, created_at: new Date().toISOString(), last_used_at: null }, ...prev]);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not create the token.');
    }
  };
  const revoke = async (t: DeviceToken) => {
    try {
      await api.revokeToken(t.id);
      setTokens((prev) => prev.filter((x) => x.id !== t.id));
      setMade(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not revoke the token.');
    }
  };

  return (
    <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div class="modal" role="dialog" aria-label="Connect an AI client">
        <div class="mh">
          Connect an AI client
          <button class="tb" style={{ marginLeft: 'auto' }} onClick={close}>
            Done
          </button>
        </div>
        <div class="mb">
          {error && <div class="err">{error}</div>}
          <div class="hint" style={{ paddingLeft: 0 }}>
            Claude Code, Cursor or any MCP client can read this paper, compile it, search it and its bibliography, and
            propose edits. The model runs on your side with your own account. Its edits arrive here as suggestions marked
            “via” the agent, and nothing changes until an author accepts them.
          </div>

          <ol class="steps">
            <li>
              <b>Create a token</b> for the client. It is shown once.
              <div class="field">
                <input
                  placeholder="Client name"
                  value={label}
                  onInput={(e) => setLabel((e.target as HTMLInputElement).value)}
                  aria-label="Client name"
                />
                <button class="tb primary" onClick={() => void create()} disabled={!label.trim()}>
                  Create token
                </button>
              </div>
            </li>
            <li>
              <b>Paste the command</b> into a terminal. For Claude Code:
              {made ? (
                <div class="link">
                  <span style={{ fontFamily: 'var(--mono)', fontSize: 11 }}>{mcpAddCommand(id, made)}</span>
                  <button class="tb" onClick={() => copy(mcpAddCommand(id, made))}>
                    Copy
                  </button>
                </div>
              ) : (
                <div class="hint" style={{ paddingLeft: 0 }}>
                  The command appears here after step 1.
                </div>
              )}
              <div class="hint" style={{ paddingLeft: 0 }}>
                Other MCP clients take the address below and the token as a Bearer header.
              </div>
              <div class="link">
                <span>{api.mcpUrl(id)}</span>
                <button class="tb" onClick={() => copy(api.mcpUrl(id))}>
                  Copy address
                </button>
              </div>
              {made && (
                <div class="link">
                  <span>{made}</span>
                  <button class="tb" onClick={() => copy(made)}>
                    Copy token
                  </button>
                </div>
              )}
            </li>
            <li>
              <b>Ask for help</b> in the client, for example “fix the build errors in my Galley paper” or “find weak
              citations in section 2”. Suggestions appear in the Comments drawer.
            </li>
          </ol>

          {made && (
            <>
              <div style={{ marginTop: 12, fontWeight: 500 }}>Or run a local model</div>
              <div class="link">
                <span style={{ fontFamily: 'var(--mono)', fontSize: 11 }}>{runnerCommand(id, made)}</span>
                <button class="tb" onClick={() => copy(runnerCommand(id, made))}>
                  Copy
                </button>
              </div>
              <div class="hint" style={{ paddingLeft: 0 }}>
                Needs Ollama, or any OpenAI-compatible endpoint, and a tool-capable model of about 7B or more. Smaller models
                tend to stop early.
              </div>
            </>
          )}

          {tokens.length > 0 && <div style={{ marginTop: 16, fontWeight: 500 }}>Your tokens</div>}
          {tokens.map((t) => (
            <div class="link" key={t.id}>
              <span>
                {t.label} · {t.last_used_at ? `used ${new Date(t.last_used_at).toLocaleDateString()}` : 'never used'}
              </span>
              <button class="tb" onClick={() => void revoke(t)}>
                Revoke
              </button>
            </div>
          ))}
          {tokens.length > 0 && (
            <div class="hint" style={{ paddingLeft: 0 }}>
              A token works on every project you can open, with the role you hold there. Revoke one you no longer use.
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
