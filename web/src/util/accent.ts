export const DEFAULT_ACCENT = '#c6402a';
export const ACCENT_PRESETS = [
  { name: 'Terracotta', color: DEFAULT_ACCENT },
  { name: 'Amber', color: '#d59a24' },
  { name: 'Moss', color: '#4c8057' },
  { name: 'Teal', color: '#168b86' },
  { name: 'Ocean', color: '#287bcc' },
  { name: 'Indigo', color: '#6861d8' },
  { name: 'Plum', color: '#a04ba5' },
  { name: 'Rose', color: '#cf507b' },
];

export function normalizeHex(value: string): string | null {
  const hex = value.trim().replace(/^#/, '');
  if (/^[0-9a-f]{3}$/i.test(hex)) return '#' + [...hex].map((c) => c + c).join('').toLowerCase();
  return /^[0-9a-f]{6}$/i.test(hex) ? `#${hex.toLowerCase()}` : null;
}

function rgb(hex: string): number[] {
  return [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16));
}

function mix(a: string, b: string, amount: number): string {
  const other = rgb(b);
  return '#' + rgb(a).map((channel, i) => Math.round(channel * (1 - amount) + other[i]! * amount).toString(16).padStart(2, '0')).join('');
}

function luminance(hex: string): number {
  const linear = rgb(hex).map((c) => {
    const channel = c / 255;
    return channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return linear[0]! * 0.2126 + linear[1]! * 0.7152 + linear[2]! * 0.0722;
}

export function contrast(a: string, b: string): number {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

export function onAccent(color: string): string {
  return contrast(color, '#ffffff') >= contrast(color, '#000000') ? '#ffffff' : '#000000';
}

/** Keep the chosen fill intact; adapt text and focus rings for the current theme's surfaces. */
export function accentTokens(value: string, theme: 'light' | 'dark'): Record<string, string> {
  const color = normalizeHex(value) ?? DEFAULT_ACCENT;
  const surfaces = theme === 'dark' ? ['#1b1a17', '#23221e', '#181714'] : ['#f6f3ec', '#fbf9f4', '#ece7dc'];
  const soft = mix(surfaces[1]!, color, theme === 'dark' ? 0.18 : 0.12);
  const target = theme === 'dark' ? '#ffffff' : '#000000';
  let ink = color;
  for (let step = 0; step <= 100; step++) {
    ink = mix(color, target, step / 100);
    if ([...surfaces, soft].every((surface) => contrast(ink, surface) >= 4.5)) break;
  }
  const foreground = onAccent(color);
  return {
    '--accent': color,
    '--accent-ink': ink,
    '--accent-soft': soft,
    '--accent-hover': mix(color, foreground === '#ffffff' ? '#000000' : '#ffffff', 0.12),
    '--on-accent': foreground,
    '--sel': soft,
  };
}
