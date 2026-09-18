import { describe, expect, it } from 'vitest';
import { collectTasks, removalRange } from './tasks';

const text = 'Intro text. % TODO(@julia): cite Big Bird\n% FIXME: broken figure\nSee \\todo{@ana check units} here.\nend';

describe('collectTasks', () => {
  it('finds comment and macro notes with assignees', () => {
    const t = collectTasks('main.tex', text);
    expect(t.map((x) => [x.kind, x.who, x.text, x.line, x.src])).toEqual([
      ['TODO', 'julia', 'cite Big Bird', 1, 'comment'],
      ['FIXME', '', 'broken figure', 2, 'comment'],
      ['TODO', 'ana', 'check units', 3, 'macro'],
    ]);
  });
});

describe('removalRange', () => {
  const apply = (t: string, r: { from: number; to: number }) => t.slice(0, r.from) + t.slice(r.to);
  it('removes a whole-line note with its newline, and an inline note with its leading space', () => {
    const t = collectTasks('main.tex', text);
    expect(apply(text, removalRange(text, t[1]!)!)).toBe('Intro text. % TODO(@julia): cite Big Bird\nSee \\todo{@ana check units} here.\nend');
    expect(apply(text, removalRange(text, t[0]!)!)).toBe('Intro text.\n% FIXME: broken figure\nSee \\todo{@ana check units} here.\nend');
    expect(apply(text, removalRange(text, t[2]!)!)).toBe('Intro text. % TODO(@julia): cite Big Bird\n% FIXME: broken figure\nSee here.\nend');
  });
  it('returns null when the line changed underneath', () => {
    const t = collectTasks('main.tex', text)[0]!;
    expect(removalRange('something else\n', t)).toBeNull();
  });
});
