import { describe, expect, it } from 'vitest';
import { authorSurnames, complianceChecks, fontSize, type Venue } from './compliance';

// Mirrors applyFix: the first match of `find` on the line becomes `text`.
const applyReplace = (line: string, find: string, text: string) => line.replace(new RegExp(find), () => text);

const neurips: Venue = {
  id: 'neurips',
  name: 'NeurIPS 2026',
  short: 'NeurIPS',
  pages: 9,
  font_size: '10pt',
  anonymous: true,
  engine: 'TeX Live 2025',
};
const acl: Venue = {
  ...neurips,
  id: 'acl',
  name: 'ACL Rolling Review',
  short: 'ACL',
  pages: 8,
  font_size: '11pt',
};
const arxiv: Venue = {
  id: 'arxiv',
  name: 'arXiv',
  short: 'arXiv',
  anonymous: false,
  engine: 'TeX Live 2025',
};
const main =
  "\\documentclass[11pt,a4paper]{article}\n\\author{Pat Smith\\thanks{Uni} \\and Lee O'Neil}\n\\begin{document}\nSmith (2020) showed it.\n\\bibliography{refs}\n\\end{document}\n";
const base = {
  mainFile: 'main.tex',
  mainText: main,
  otherTex: [],
  pages: 10,
  openTasks: 2,
  buildProblems: 0,
};

describe('compliance', () => {
  it('reads the font size and author surnames', () => {
    expect(fontSize(main)).toEqual({ size: '11pt', line: 1 });
    expect(fontSize('\\documentclass{article}')).toEqual({
      size: '10pt',
      line: 1,
    });
    expect(authorSurnames(main)).toEqual(['Smith', "O'Neil"]);
  });

  it('flags every problem for an anonymous venue and offers a font fix', () => {
    const c = complianceChecks({ ...base, venue: neurips });
    expect(c.map((x) => x.ok)).toEqual([false, false, false, false, true, true]);
    expect(c[2]!.text).toContain('Smith');
    const fix = c[1]!.fix!;
    expect(fix.kind).toBe('replace');
    if (fix.kind === 'replace') {
      expect(applyReplace(main.split('\n')[0]!, fix.find, fix.text)).toBe('\\documentclass[10pt,a4paper]{article}');
    }
  });

  it('adds an option list when the class has none, and marks n/a checks', () => {
    const plain = '\\documentclass{article}\n\\begin{document}\nx\n\\end{document}';
    // Without an option the class is 10pt: right for NeurIPS, wrong for ACL.
    expect(
      complianceChecks({
        ...base,
        mainText: plain,
        venue: neurips,
        pages: 3,
        openTasks: 0,
      })[1]!.ok,
    ).toBe(true);
    const fix = complianceChecks({
      ...base,
      mainText: plain,
      venue: acl,
      pages: 3,
      openTasks: 0,
    })[1]!.fix!;
    expect(fix.kind).toBe('replace');
    if (fix.kind === 'replace')
      expect(applyReplace('\\documentclass{article}', fix.find, fix.text)).toBe('\\documentclass[11pt]{article}');
    const a = complianceChecks({
      ...base,
      venue: arxiv,
      pages: null,
      buildProblems: null,
    });
    expect(a.filter((x) => x.na).length).toBe(3);
    expect(a[4]!.text).toBe('Not built yet');
  });
});
