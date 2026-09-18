// Project settings that live in .galley/project.toml (SPEC §13.7): deadline, venue, word budgets.
import { api, type SettingsPatch } from '../api';
import { messageOf } from './auth';
import { project, showToast } from './store';

export async function saveSettings(patch: SettingsPatch): Promise<boolean> {
  const id = project.value?.id;
  if (!id) return false;
  try {
    const meta = await api.updateSettings(id, patch);
    project.value = { ...project.value!, ...meta };
    return true;
  } catch (e) {
    showToast(messageOf(e, 'Could not save the setting.'));
    return false;
  }
}

/** Days until the deadline, counting today as 0. Null when none is set. */
export function daysLeft(deadline: string | null | undefined, now = new Date()): number | null {
  if (!deadline) return null;
  const [y, m, d] = deadline.split('-').map(Number);
  if (!y || !m || !d) return null;
  const due = Date.UTC(y, m - 1, d);
  const today = Date.UTC(now.getFullYear(), now.getMonth(), now.getDate());
  return Math.round((due - today) / 86_400_000);
}

export async function setDeadline(): Promise<void> {
  const current = project.value?.deadline ?? '';
  const v = window.prompt('Deadline (YYYY-MM-DD, empty to clear)', current || new Date().toISOString().slice(0, 10));
  if (v === null) return;
  if (await saveSettings({ deadline: v.trim() })) showToast(v.trim() ? 'Deadline set.' : 'Deadline cleared.');
}

export async function setBudget(title: string): Promise<void> {
  const budgets = { ...(project.value?.budgets ?? {}) };
  const current = budgets[title];
  const v = window.prompt(`Word budget for “${title}” (empty to clear)`, current ? String(current) : '');
  if (v === null) return;
  const n = Number.parseInt(v, 10);
  if (v.trim() === '' || Number.isNaN(n) || n <= 0) delete budgets[title];
  else budgets[title] = n;
  await saveSettings({ budgets });
}

export async function toggleFigureCache(): Promise<void> {
  const on = project.value?.figure_cache !== false;
  if (await saveSettings({ figure_cache: !on })) {
    showToast(on ? 'Figure cache off. Every build draws every figure.' : 'Figure cache on. It takes effect from the next build.');
  }
}

export async function clearFigureCache(): Promise<void> {
  const id = project.value?.id;
  if (!id) return;
  try {
    const r = await api.clearFigures(id);
    showToast(r.removed ? `Cleared ${r.removed} cached figure${r.removed === 1 ? '' : 's'}. The next build regenerates them.` : 'The figure cache was already empty.');
  } catch (e) {
    showToast(messageOf(e, 'Could not clear the figure cache.'));
  }
}
