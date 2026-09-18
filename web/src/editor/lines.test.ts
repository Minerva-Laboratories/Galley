import { describe, expect, it } from 'vitest';
import { lineEndOffset, lineRange } from './lines';

describe('line helpers', () => {
  const text = 'ab\ncde\n\nf';
  it('finds line ends', () => {
    expect(lineEndOffset(text, 1)).toBe(2);
    expect(lineEndOffset(text, 2)).toBe(6);
    expect(lineEndOffset(text, 3)).toBe(7);
    expect(lineEndOffset(text, 4)).toBe(9);
    expect(lineEndOffset(text, 5)).toBeNull();
    expect(lineEndOffset(text, 0)).toBeNull();
  });
  it('finds line ranges', () => {
    expect(lineRange(text, 2)).toEqual({ from: 3, to: 6, text: 'cde' });
    expect(lineRange(text, 3)).toEqual({ from: 7, to: 7, text: '' });
    expect(lineRange(text, 9)).toBeNull();
  });
});
