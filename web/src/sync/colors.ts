// Collaborator colours: a small fixed palette, chosen by a stable hash of the display name so a
// person keeps the same colour across tabs and sessions. All pass 4.5:1 as text on Paper.

const PALETTE: { color: string; light: string }[] = [
  { color: '#1d9e75', light: '#1d9e7533' },
  { color: '#3d5fc4', light: '#3d5fc433' },
  { color: '#b8541f', light: '#b8541f33' },
  { color: '#7a3e9d', light: '#7a3e9d33' },
  { color: '#0e7c86', light: '#0e7c8633' },
  { color: '#a33a6e', light: '#a33a6e33' },
];

export function hashName(name: string): number {
  let h = 2166136261;
  for (let i = 0; i < name.length; i++) {
    h ^= name.charCodeAt(i);
    h = Math.imul(h, 16777619) >>> 0;
  }
  return h;
}

export function colorFor(name: string): { color: string; light: string } {
  const entry = PALETTE[hashName(name || 'anonymous') % PALETTE.length];
  return entry ?? PALETTE[0]!;
}

export function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (parts.length === 0) return '?';
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase();
  return (parts[0]![0]! + parts[parts.length - 1]![0]!).toUpperCase();
}
