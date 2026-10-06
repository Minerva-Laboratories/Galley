import { useEffect, useState } from 'preact/hooks';
import { api, ApiError, type DeviceToken } from '../api';
import { connectOpen, project, showToast } from '../store/store';
import { claudeSetup, codexConfig, codexSetup, type TerminalKind } from '../util/mcpSetup';

const CLIENTS = { codex: 'OpenAI Codex', claude: 'Claude Code', other: 'Other MCP client' } as const;
type Client = keyof typeof CLIENTS;

function runnerCommand(projectId: string, token: string): string {
  return `galley agent run --url ${api.mcpUrl(projectId)} --token ${token} --model qwen2.5-coder:14b`;
}

/** Connecting an AI client is a main feature, so it has its own panel rather than a corner of Share.
 * Any signed-in author can connect: a token belongs to the person and reaches every project they can
 * open, with the role they hold there. */
export function ConnectAiModal() {
  const id = project.value?.id;
  const [tokens, setTokens] = useState<DeviceToken[]>([]);
  const [client, setClient] = useState<Client>('codex');
  const [terminal, setTerminal] = useState<TerminalKind>('posix');
  const [label, setLabel] = useState<string>(CLIENTS.codex);
  const [creating, setCreating] = useState(false);
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
    if (creating) return;
    setCreating(true);
    try {
      const r = await api.createToken(label.trim());
      setMade(r.token);
      setTokens((prev) => [{ id: r.id, label: r.label, created_at: new Date().toISOString(), last_used_at: null }, ...prev]);
      setError(null);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : 'Could not create the token.');
    } finally {
      setCreating(false);
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

  const command = made ? client === 'codex' ? codexSetup(id, api.mcpUrl(id), made, terminal)
    : claudeSetup(id, api.mcpUrl(id), made) : '';

  return (
    <div class="overlay" onMouseDown={(e) => e.target === e.currentTarget && close()}>
      <div class="modal connect-modal" role="dialog" aria-label="Connect an AI client">
        <div class="mh">
          Connect an AI client
          <button class="tb" style={{ marginLeft: 'auto' }} onClick={close}>
            Done
          </button>
        </div>
        <div class="mb">
          {error && <div class="err">{error}</div>}
          <div class="hint" style={{ paddingLeft: 0 }}>
            OpenAI Codex, Claude Code, Cursor or any MCP client can read this paper, compile it, search it and its
            bibliography, and propose edits. Your client uses your own AI account. Edits arrive here as suggestions
            marked “via” the agent, and nothing changes until an author accepts them.
          </div>

          <div class="field">
            <label for="ai-client">AI client</label>
            <select id="ai-client" value={client} onChange={(e) => {
              const next = e.currentTarget.value as Client;
              if (label === CLIENTS[client]) setLabel(CLIENTS[next]);
              setClient(next);
            }}>
              {Object.entries(CLIENTS).map(([value, name]) => <option value={value}>{name}</option>)}
            </select>
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
                <button class="tb primary" onClick={() => void create()} disabled={!label.trim() || creating}>
                  {creating ? 'Creating…' : 'Create token'}
                </button>
              </div>
            </li>
            <li>
              {client !== 'other' ? (
                <>
                  <b>Connect {CLIENTS[client]}.</b>
                  {client === 'codex' ? (
                    <>
                      <div class="hint" style={{ paddingLeft: 0 }}>
                        With the Codex CLI installed, run these commands in your terminal. The last line starts Codex
                        with the token available. Use your Galley token, not an OpenAI API key.
                      </div>
                      <div class="field">
                        <label for="codex-terminal">Terminal</label>
                        <select id="codex-terminal" value={terminal} onChange={(e) => setTerminal(e.currentTarget.value as TerminalKind)}>
                          <option value="posix">macOS / Linux (Bash or Zsh)</option>
                          <option value="powershell">Windows (PowerShell)</option>
                        </select>
                      </div>
                    </>
                  ) : <div class="hint" style={{ paddingLeft: 0 }}>Run this command in a Bash or Zsh terminal.</div>}
                  {made ? (
                    <div class="mcp-command">
                      <pre>{command}</pre>
                      <button class="tb" onClick={() => copy(command)}>Copy commands</button>
                    </div>
                  ) : <div class="hint" style={{ paddingLeft: 0 }}>The commands appear after you create a token in step 1.</div>}
                  {client === 'codex' && (
                    <>
                      <div class="hint" style={{ paddingLeft: 0 }}>
                        In Codex, use <code>/mcp</code> to check the connection. If you open a new terminal, set the token
                        variable again before starting Codex.
                      </div>
                      <details class="codex-config">
                        <summary>Using the Codex app or IDE extension?</summary>
                        <p>Add this to your personal <code>~/.codex/config.toml</code> (Windows: <code>%USERPROFILE%\.codex\config.toml</code>), then restart the app or extension and open a new chat.</p>
                        {made ? <div class="mcp-command">
                          <pre>{codexConfig(id, api.mcpUrl(id), made)}</pre>
                          <button class="tb" onClick={() => copy(codexConfig(id, api.mcpUrl(id), made))}>Copy configuration</button>
                        </div> : <p>Create a token first to see the configuration.</p>}
                        <p>This personal configuration contains your token. Keep it out of shared repositories.
                          If this server already exists, update its section instead of adding a duplicate.</p>
                      </details>
                      <a class="linkish" href="https://developers.openai.com/codex/mcp/" target="_blank" rel="noopener noreferrer">Official OpenAI Codex MCP guide</a>
                    </>
                  )}
                </>
              ) : <><b>Configure your MCP client.</b><div class="hint" style={{ paddingLeft: 0 }}>Use Streamable HTTP with the address below and an <code>Authorization: Bearer &lt;token&gt;</code> header.</div></>}
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
