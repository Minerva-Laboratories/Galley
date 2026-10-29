// Auth actions. Kept out of components so the gate and forms share one source of truth.
import { api, ApiError } from '../api';
import { forgetUserCopies } from '../sync/docs';
import { authReady, currentUser, needsSetup, publicSignup, setDisplayName, ssoLabel } from './store';

export async function loadMe(): Promise<void> {
  try {
    const me = await api.me();
    currentUser.value = me.user;
    needsSetup.value = me.needs_setup;
    publicSignup.value = me.public_signup;
    ssoLabel.value = me.oidc?.label ?? null;
    // The server holds the chosen name for guests too, so it is authoritative for both.
    if (me.user) setDisplayName(me.user.name);
  } catch {
    // Leave the SPA on the sign-in screen. The server may be starting.
    currentUser.value = null;
  } finally {
    authReady.value = true;
  }
}

export async function login(email: string, password: string): Promise<void> {
  const { user } = await api.login(email, password);
  currentUser.value = user;
  setDisplayName(user.name);
}

export async function signup(email: string, name: string, password: string): Promise<void> {
  const { user } = await api.signup(email, name, password);
  currentUser.value = user;
  needsSetup.value = false;
  setDisplayName(user.name);
}

// Set while this tab's own Sign out is in flight. The server's signed_out frame on the event socket
// can arrive before the HTTP answer and must not show "You were signed out." here. Once the answer
// is in, the local user is gone, and a late frame finds no one to sign out.
let signingOut = false;
export function isSigningOut(): boolean {
  return signingOut;
}

export async function logout(): Promise<void> {
  const previousUserId = currentUser.value?.id ?? null;
  signingOut = true;
  let confirmed = false;
  try {
    await api.logout();
    confirmed = true;
  } catch {
    // Even if the call fails, drop the local identity.
  }
  dropLocalIdentity();
  signingOut = false;
  // An explicit Sign out deletes this account's offline copies in this browser, and never another
  // account's. Edits not yet synced are lost by this click.
  if (confirmed && previousUserId) void forgetUserCopies(previousUserId);
}

/** Forget the signed-in user locally, without calling the server: after this tab's Sign out, or
 *  when the server reports that the session ended elsewhere. Offline copies are kept here. */
export function dropLocalIdentity(): void {
  currentUser.value = null;
}

/** Rename the signed-in user, guest or not, so commits, comments and the member list follow. */
export async function renameMe(name: string): Promise<void> {
  const user = await api.setName(name);
  currentUser.value = user;
  setDisplayName(user.name);
}

export function messageOf(e: unknown, fallback: string): string {
  return e instanceof ApiError ? e.message : fallback;
}
