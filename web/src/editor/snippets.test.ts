import { describe, expect, it } from 'vitest';
import { autoLabelChanges, slug } from './autolabel';

describe('slug', () => {
  it('strips commands and punctuation, keeps four words', () => {
    expect(slug('Results on the \\textbf{large} benchmark: a study')).toBe('results-on-the-large');
    expect(slug('')).toBe('');
  });
});

describe('autoLabelChanges', () => {
  it('follows the caption while % auto is present', () => {
    const t = '\\begin{figure}\n  \\caption{Training loss over time}\n  \\label{fig:} % auto\n\\end{figure}\n';
    const c = autoLabelChanges(t);
    expect(c).toHaveLength(1);
    expect(c[0]!.insert).toBe('\\label{fig:training-loss-over-time} % auto');
    const applied = t.slice(0, c[0]!.from) + c[0]!.insert + t.slice(c[0]!.to);
    expect(autoLabelChanges(applied)).toEqual([]);
  });

  it('uses the heading for sec labels and leaves pinned labels alone', () => {
    const t = '\\section{Method}\n\\label{sec:} % auto\n\\section{Old}\n\\label{sec:custom}\n';
    const c = autoLabelChanges(t);
    expect(c).toHaveLength(1);
    expect(c[0]!.insert).toBe('\\label{sec:method} % auto');
  });

  it('does not borrow a caption from an earlier environment', () => {
    const t = '\\caption{First}\n\\label{fig:first} % auto\n\\begin{figure}\n\\includegraphics{x}\n\\label{fig:} % auto\n\\end{figure}\n';
    const c = autoLabelChanges(t);
    expect(c).toEqual([]);
  });
});
