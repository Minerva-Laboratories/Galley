import { describe, expect, it } from 'vitest';
import { sectionWords } from './latex';
import { outline, wordCount } from './latex';

describe('outline', () => {
  it('finds sections with levels and line numbers', () => {
    const text = ['\\section{Intro}', 'text', '\\subsection*{Setup}', '  \\subsubsection{Deep}'].join('\n');
    expect(outline(text)).toEqual([
      { level: 1, title: 'Intro', line: 1 },
      { level: 2, title: 'Setup', line: 3 },
      { level: 3, title: 'Deep', line: 4 },
    ]);
  });

  it('ignores commented-out headings', () => {
    expect(outline('% \\section{Nope}')).toEqual([]);
  });
});

describe('wordCount', () => {
  it('counts prose only', () => {
    expect(wordCount('We propose a mask.')).toBe(4);
    expect(wordCount('\\section{Intro} Hello world % comment here')).toBe(2);
    expect(wordCount('The mask $M_{ij}$ is one.')).toBe(4);
    expect(wordCount('\\begin{figure}\\end{figure}')).toBe(0);
    expect(wordCount('50\\% of cases')).toBe(3);
  });
});

describe('sectionWords', () => {
  it('counts prose per section up to the next heading', () => {
    const t = '\\section{One}\nalpha beta gamma\n\\subsection{Two}\ndelta \\textbf{epsilon}\n% zeta eta\n\\section{Three}\n';
    expect(sectionWords(t).map((s) => [s.title, s.words])).toEqual([
      ['One', 3],
      ['Two', 1],
      ['Three', 0],
    ]);
  });
});
