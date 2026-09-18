// The pure half of snippets with auto-labels (SPEC §13.3): the snippet table and the label rule,
// kept free of editor and store imports so they are unit-testable.
interface Snippet {
  text: string;
  /** The cursor lands right after this substring of `text`. */
  cursor: string;
}

export const SNIPPETS: Record<string, Snippet> = {
  fig: {
    text: '\\begin{figure}[t]\n  \\centering\n  \\includegraphics[width=.8\\linewidth]{}\n  \\caption{}\n  \\label{fig:} % auto\n\\end{figure}\n',
    cursor: '\\caption{',
  },
  tab: {
    text: '\\begin{table}[t]\n  \\centering\n  \\caption{}\n  \\label{tab:} % auto\n  \\begin{tabular}{lcc}\n    \\toprule\n     & & \\\\\n    \\midrule\n     & & \\\\\n    \\bottomrule\n  \\end{tabular}\n\\end{table}\n',
    cursor: '\\caption{',
  },
  eq: {
    text: '\\begin{equation}\n  \n  \\label{eq:}\n\\end{equation}\n',
    cursor: '\n  ',
  },
  sec: { text: '\\section{}\n\\label{sec:} % auto\n', cursor: '\\section{' },
  sub: {
    text: '\\subsection{}\n\\label{sec:} % auto\n',
    cursor: '\\subsection{',
  },
  cite: { text: '\\cite{}', cursor: '\\cite{' },
  todo: { text: '% TODO(@me): ', cursor: '% TODO(@me): ' },
};

/** Lowercase ASCII words, hyphen-joined, at most four. This is the prototype's rule. */
export function slug(title: string): string {
  return title
    .replace(/\\[a-zA-Z]+/g, ' ')
    .replace(/[^a-zA-Z0-9 ]/g, ' ')
    .trim()
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 4)
    .join('-');
}

const AUTO = /\\label\{(fig|tab|sec):[^}]*\} % auto/g;
const CAPTION_BEFORE = /\\caption\{([^}]*)\}[^\\]*$/;
const HEADING_BEFORE = /\\(?:sub)?section\*?\{([^}]*)\}\s*$/;

/** The rewrites that bring every `% auto` label in line with its caption or heading. */
export function autoLabelChanges(text: string): { from: number; to: number; insert: string }[] {
  const out: { from: number; to: number; insert: string }[] = [];
  for (const m of text.matchAll(AUTO)) {
    const kind = m[1]!;
    const pre = text.slice(0, m.index);
    const src = kind === 'sec' ? HEADING_BEFORE.exec(pre) : CAPTION_BEFORE.exec(pre);
    const next = `\\label{${kind}:${src ? slug(src[1]!) : ''}} % auto`;
    if (next !== m[0]) out.push({ from: m.index!, to: m.index! + m[0].length, insert: next });
  }
  return out;
}
