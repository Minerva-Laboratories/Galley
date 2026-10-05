import { describe, expect, it } from 'vitest';
import { fileTree } from './fileTree';

describe('file tree', () => {
  const files = [
    { path: 'main.tex', kind: 'text' as const, size: 0 },
    { path: 'figures/charts/result.pdf', kind: 'binary' as const, size: 20 },
    { path: 'figures/result.pdf', kind: 'binary' as const, size: 10 },
  ];
  it('groups existing files, includes empty folders and retains full paths', () => {
    const rows = fileTree(files, ['empty', 'figures', 'figures/charts'], new Set());
    expect(rows.map((r) => [r.path, r.name, r.depth])).toEqual([
      ['empty', 'empty', 0],
      ['figures', 'figures', 0],
      ['figures/charts', 'charts', 1],
      ['figures/charts/result.pdf', 'result.pdf', 2],
      ['figures/result.pdf', 'result.pdf', 1],
      ['main.tex', 'main.tex', 0],
    ]);
    expect(rows.filter((r) => r.file).map((r) => r.file)).toHaveLength(3);
  });
  it('collapses descendants without hiding siblings', () => {
    expect(fileTree(files, ['empty'], new Set(['figures'])).map((r) => r.path)).toEqual(['empty', 'figures', 'main.tex']);
    expect(fileTree(files, [], new Set(['figures/charts'])).map((r) => r.path)).toEqual(['figures', 'figures/charts', 'figures/result.pdf', 'main.tex']);
  });
});
