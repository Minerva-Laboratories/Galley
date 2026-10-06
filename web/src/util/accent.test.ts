import { describe, expect, it } from 'vitest';
import { ACCENT_PRESETS, DEFAULT_ACCENT, accentTokens, contrast, normalizeHex } from './accent';

describe('accent colors', () => {
  it('normalizes color input and rejects malformed saved values', () => {
    expect(normalizeHex(' #AbC ')).toBe('#aabbcc');
    expect(normalizeHex('287BCC')).toBe('#287bcc');
    for (const value of ['', 'red', '#abcd', '#12345678', 'url(x)', '#fffffg']) expect(normalizeHex(value)).toBeNull();
    expect(accentTokens('invalid', 'dark')['--accent']).toBe(DEFAULT_ACCENT);
  });
  it('keeps button labels and accent text readable for presets and extreme custom colors', () => {
    const colors = [...ACCENT_PRESETS.map((preset) => preset.color), '#ffffff', '#000000', '#ffff00', '#00ff00', '#ff00ff', '#808080'];
    for (const theme of ['light', 'dark'] as const) {
      const surfaces = theme === 'dark' ? ['#1b1a17', '#23221e', '#181714'] : ['#f6f3ec', '#fbf9f4', '#ece7dc'];
      for (const color of colors) {
        const tokens = accentTokens(color, theme);
        expect(tokens['--accent']).toBe(color);
        expect(contrast(tokens['--accent']!, tokens['--on-accent']!)).toBeGreaterThanOrEqual(4.5);
        expect(contrast(tokens['--accent-hover']!, tokens['--on-accent']!)).toBeGreaterThanOrEqual(4.5);
        for (const surface of [...surfaces, tokens['--accent-soft']!]) {
          expect(contrast(tokens['--accent-ink']!, surface), `${color} on ${surface}`).toBeGreaterThanOrEqual(4.5);
        }
        expect(tokens['--red']).toBeUndefined();
      }
    }
  });
});
