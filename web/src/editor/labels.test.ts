import { describe, expect, it } from 'vitest';
import { labelAt, renameInText } from './labels';

function apply(text: string, edits: { from: number; to: number; insert: string }[]) {
  return [...edits].sort((a, b) => b.from - a.from).reduce((t, e) => t.slice(0, e.from) + e.insert + t.slice(e.to), text);
}

describe('renameInText', () => {
  it('renames every referencing command and only whole keys', () => {
    const t = '\\label{sec:method}\nSee \\ref{sec:method} and \\autoref{sec:method-old} and \\cref{eq:1,sec:method,fig:x}.';
    const e = renameInText(t, 'sec:method', 'sec:approach');
    expect(e).toHaveLength(3);
    expect(apply(t, e)).toBe('\\label{sec:approach}\nSee \\ref{sec:approach} and \\autoref{sec:method-old} and \\cref{eq:1,sec:approach,fig:x}.');
  });

  it('is a no-op for the same name or no matches', () => {
    expect(renameInText('\\ref{a}', 'a', 'a')).toEqual([]);
    expect(renameInText('\\ref{a}', 'b', 'c')).toEqual([]);
  });
});

describe('labelAt', () => {
  it('prefers the command under the cursor, else the first on the line', () => {
    const line = 'as in \\ref{fig:one} and \\cref{tab:two,tab:three}';
    expect(labelAt(line, 30)).toBe('tab:two');
    expect(labelAt(line, 2)).toBe('fig:one');
    expect(labelAt('plain text', 3)).toBeNull();
  });
});
