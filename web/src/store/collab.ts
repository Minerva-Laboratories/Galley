// Comment and suggestion actions. Accepting a suggestion applies its edit through the CRDT, so it
// syncs and undoes like any edit (principle 4).
import { api } from '../api';
import type { Comment, Suggestion } from '../api';
import { encodeAnchor, resolveAnchor } from '../sync/anchor';
import { openDoc } from '../sync/docs';
import { messageOf } from './auth';
import { comments, currentSession, currentUser, displayName, editorView, project, showToast, suggestions } from './store';

/** The active editor's selection as {file, from, to, text}, or null if no file is open. */
export function selection(): { file: string; from: number; to: number; text: string } | null {
  const session = currentSession.value;
  const view = editorView.value;
  if (!session || !view) return null;
  const sel = view.state.selection.main;
  return { file: session.path, from: sel.from, to: sel.to, text: view.state.doc.sliceString(sel.from, sel.to) };
}

export async function loadCollab(projectId: string): Promise<void> {
  try {
    const [c, s] = await Promise.all([api.comments(projectId), api.suggestions(projectId)]);
    comments.value = c;
    suggestions.value = s;
  } catch {
    // The drawers show an empty state. A reconnect refetches.
  }
}

export async function addComment(body: string): Promise<void> {
  const id = project.value?.id;
  const sel = selection();
  const session = currentSession.value;
  if (!id || !sel || !session) return;
  const anchor = encodeAnchor(session.ytext, sel.from);
  const quote = sel.text.slice(0, 200) || null;
  try {
    const comment = await api.addComment(id, sel.file, anchor, quote, body);
    upsertComment(comment);
  } catch (e) {
    showToast(messageOf(e, 'Could not add the comment.'));
  }
}

export async function resolveComment(commentId: string, resolved: boolean): Promise<void> {
  const id = project.value?.id;
  if (!id) return;
  try {
    await api.resolveComment(id, commentId, resolved);
    comments.value = comments.value.map((c) => (c.id === commentId ? { ...c, resolved } : c));
  } catch (e) {
    showToast(messageOf(e, 'Could not update the comment.'));
  }
}

export async function addSuggestion(replacement: string): Promise<void> {
  const id = project.value?.id;
  const sel = selection();
  const session = currentSession.value;
  if (!id || !sel || !session) return;
  if (sel.from === sel.to) {
    showToast('Select the text you want to change first.');
    return;
  }
  const anchor = encodeAnchor(session.ytext, sel.from);
  const anchor_end = encodeAnchor(session.ytext, sel.to);
  try {
    const s = await api.addSuggestion(id, { file: sel.file, anchor, anchor_end, quote: sel.text.slice(0, 400), replacement });
    upsertSuggestion(s);
  } catch (e) {
    showToast(messageOf(e, 'Could not add the suggestion.'));
  }
}

export async function acceptSuggestion(s: Suggestion): Promise<void> {
  const id = project.value?.id;
  if (!id) return;
  const session = openDoc(id, s.file, displayName.value || currentUser.value?.name || 'Anonymous');
  const from = resolveAnchor(session.ydoc, s.anchor);
  const to = resolveAnchor(session.ydoc, s.anchor_end);
  if (from === null || to === null || to < from) {
    showToast('The text around this suggestion changed. Reject it and suggest again.');
    return;
  }
  try {
    await api.setSuggestionStatus(id, s.id, 'accepted');
    session.ydoc.transact(() => {
      if (to > from) session.ytext.delete(from, to - from);
      if (s.replacement) session.ytext.insert(from, s.replacement);
    });
    markSuggestion(s.id, 'accepted');
    showToast('Suggestion applied.');
  } catch (e) {
    showToast(messageOf(e, 'Could not accept the suggestion.'));
  }
}

export async function rejectSuggestion(suggestionId: string): Promise<void> {
  const id = project.value?.id;
  if (!id) return;
  try {
    await api.setSuggestionStatus(id, suggestionId, 'rejected');
    markSuggestion(suggestionId, 'rejected');
  } catch (e) {
    showToast(messageOf(e, 'Could not reject the suggestion.'));
  }
}

export function upsertComment(comment: Comment): void {
  comments.value = [...comments.value.filter((c) => c.id !== comment.id), comment];
}

export function upsertSuggestion(s: Suggestion): void {
  suggestions.value = [...suggestions.value.filter((x) => x.id !== s.id), s];
}

export function markSuggestion(suggestionId: string, status: 'accepted' | 'rejected'): void {
  suggestions.value = suggestions.value.map((x) => (x.id === suggestionId ? { ...x, status } : x));
}
