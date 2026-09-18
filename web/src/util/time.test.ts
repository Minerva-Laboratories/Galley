import { describe, expect, it } from 'vitest';
import { ago } from './time';

describe('ago', () => {
  const now = Date.parse('2026-09-10T12:00:00Z');
  it('formats relative times', () => {
    expect(ago('2026-09-10T11:59:50Z', now)).toBe('just now');
    expect(ago('2026-09-10T11:55:00Z', now)).toBe('5 min ago');
    expect(ago('2026-09-10T09:00:00Z', now)).toBe('3 h ago');
    expect(ago('2026-09-09T12:00:00Z', now)).toBe('yesterday');
    expect(ago('2026-09-05T12:00:00Z', now)).toBe('5 days ago');
    expect(ago('garbage', now)).toBe('');
  });
});
