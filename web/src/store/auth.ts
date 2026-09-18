// Auth actions. Kept out of components so the gate and forms share one source of truth.
import { api, ApiError } from '../api';
import { authReady, currentUser, needsSetup, publicSignup, setDisplayName } from './store';

export async function loadMe(): Promise<void> {
  try {
    const me = await api.me();
    currentUser.value = me.user;
    needsSetup.value = me.needs_setup;
    publicSignup.value = me.public_signup;
    if (me.user && !me.user.is_guest) setDisplayName(me.user.name);
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

export async function logout(): Promise<void> {
  try {
    await api.logout();
  } catch {
    // Even if the call fails, drop the local identity.
  }
  currentUser.value = null;
}

export function messageOf(e: unknown, fallback: string): string {
  return e instanceof ApiError ? e.message : fallback;
}
