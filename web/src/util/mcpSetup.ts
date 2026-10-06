export type TerminalKind = 'posix' | 'powershell';

function quote(value: string, terminal: TerminalKind): string {
  return "'" + (terminal === 'powershell' ? value.replaceAll("'", "''") : value.replaceAll("'", "'\"'\"'")) + "'";
}

export function codexSetup(projectId: string, url: string, token: string, terminal: TerminalKind): string {
  const variable = `GALLEY_${projectId.replace(/[^a-z0-9]/gi, '_').toUpperCase()}_TOKEN`;
  const env = terminal === 'powershell' ? `$env:${variable} = ${quote(token, terminal)}` : `export ${variable}=${quote(token, terminal)}`;
  return `${env}\ncodex mcp add ${quote(`galley-${projectId}`, terminal)} --url ${quote(url, terminal)} --bearer-token-env-var ${variable}\ncodex`;
}

/** Static headers also work for desktop clients that do not inherit a terminal's environment. */
export function codexConfig(projectId: string, url: string, token: string): string {
  return `[mcp_servers.${JSON.stringify(`galley-${projectId}`)}]\nurl = ${JSON.stringify(url)}\nhttp_headers = { Authorization = ${JSON.stringify(`Bearer ${token}`)} }`;
}

export function claudeSetup(projectId: string, url: string, token: string): string {
  return `claude mcp add --transport http ${quote(`galley-${projectId}`, 'posix')} ${quote(url, 'posix')} --header ${quote(`Authorization: Bearer ${token}`, 'posix')}`;
}
