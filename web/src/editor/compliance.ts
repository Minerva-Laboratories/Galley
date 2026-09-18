// Submission compliance checks (SPEC §13.8), pure so they are unit-testable. The drawer feeds in
// the live main file, the other .tex bodies, the venue preset, and facts from the last build.
import type { Fix } from '../api';

export interface Venue {
  id: string;
  name: string;
  short: string;
  pages?: number | null;
  font_size?: string | null;
  anonymous: boolean;
  engine: string;
}

export interface Check {
  ok: boolean;
  /** Not applicable to this venue. Shown as a dash, never counted as failing. */
  na?: boolean;
  text: string;
  hint?: string;
  fix?: Fix;
}

export interface ComplianceInput {
  mainFile: string;
  mainText: string;
  /** Every other .tex file's text. */
  otherTex: string[];
  venue: Venue;
  /** Pages in the last PDF, when a build has counted them. */
  pages: number | null;
  openTasks: number;
  /** Errors plus warnings in the last build. Null before any build. */
  buildProblems: number | null;
}

const DOCCLASS = /\\documentclass(\[([^\]]*)\])?\{/;
const FIRST_PERSON =
  /\b(our (previous|prior|earlier) (work|paper|study)|we (previously|earlier) (showed|proposed|introduced))\b/i;

export function fontSize(mainText: string): { size: string; line: number } | null {
  const lines = mainText.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const m = DOCCLASS.exec(lines[i]!);
    if (!m) continue;
    const opt = (m[2] ?? '')
      .split(',')
      .map((x) => x.trim())
      .find((x) => /^\d+pt$/.test(x));
    return { size: opt ?? '10pt', line: i + 1 };
  }
  return null;
}

/** Surnames from `\author{A B \and C D}`, with `\thanks{…}` and other commands removed. */
export function authorSurnames(mainText: string): string[] {
  const m = /\\author\{((?:[^{}]|\{[^{}]*\})*)\}/.exec(mainText);
  if (!m) return [];
  return m[1]!
    .replace(/\\thanks\{[^}]*\}/g, ' ')
    .split(/\\and|\\AND|,/)
    .map(
      (a) =>
        a
          .replace(/\\[a-zA-Z]+/g, ' ')
          .replace(/[{}\\~]/g, ' ')
          .trim()
          .split(/\s+/)
          .pop() ?? '',
    )
    .filter((s) => /^[\p{L}][\p{L}'-]+$/u.test(s));
}

export function complianceChecks(i: ComplianceInput): Check[] {
  const v = i.venue;
  const checks: Check[] = [];
  const bodyStart = i.mainText.indexOf('\\begin{document}');
  const body = (bodyStart >= 0 ? i.mainText.slice(bodyStart) : '') + '\n' + i.otherTex.join('\n');
  const stripped = body.replace(/(^|[^\\])%.*$/gm, '$1');

  if (!v.pages) {
    checks.push({ ok: true, na: true, text: 'No page limit' });
  } else if (i.pages === null) {
    checks.push({
      ok: false,
      text: `Page limit: ${v.pages} pages`,
      hint: 'Build once to count the pages.',
    });
  } else {
    checks.push({
      ok: i.pages <= v.pages,
      text: `Page limit: ${i.pages} of ${v.pages} pages`,
      hint: i.pages > v.pages ? 'Try the Tighten agent on the longest section, or check figure sizes in draft mode.' : undefined,
    });
  }

  const fs = fontSize(i.mainText);
  if (!v.font_size) {
    checks.push({
      ok: true,
      na: true,
      text: `Font size: ${fs?.size ?? 'unknown'}`,
    });
  } else if (!fs) {
    checks.push({
      ok: false,
      text: `Font size: no \\documentclass found (venue requires ${v.font_size})`,
    });
  } else {
    const ok = fs.size === v.font_size;
    const line = i.mainText.split('\n')[fs.line - 1] ?? '';
    const hasOption = /\\documentclass\[[^\]]*\d+pt/.test(line);
    const fix: Fix | undefined = ok
      ? undefined
      : hasOption
        ? {
            kind: 'replace',
            label: `Set ${v.font_size}`,
            file: i.mainFile,
            line: fs.line,
            find: '\\d+pt',
            text: v.font_size,
          }
        : /\\documentclass\[/.test(line)
          ? {
              kind: 'replace',
              label: `Set ${v.font_size}`,
              file: i.mainFile,
              line: fs.line,
              find: '\\\\documentclass\\[',
              text: `\\documentclass[${v.font_size},`,
            }
          : {
              kind: 'replace',
              label: `Set ${v.font_size}`,
              file: i.mainFile,
              line: fs.line,
              find: '\\\\documentclass\\{',
              text: `\\documentclass[${v.font_size}]{`,
            };
    checks.push({
      ok,
      text: `Font size: ${fs.size}${ok ? '' : ` (venue requires ${v.font_size})`}`,
      fix,
    });
  }

  if (!v.anonymous) {
    checks.push({ ok: true, na: true, text: 'Anonymous review not required' });
  } else {
    const leak = authorSurnames(i.mainText).find((s) =>
      new RegExp(`\\b${s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`).test(stripped),
    );
    const selfRef = FIRST_PERSON.test(stripped);
    checks.push({
      ok: !leak && !selfRef,
      text: leak
        ? `Anonymization: author name “${leak}” appears in the body`
        : selfRef
          ? 'Anonymization: first-person reference to prior work'
          : 'Anonymization: no author names or self-references found',
      hint: leak || selfRef ? 'Refer to your own prior work in the third person, and keep names out of the text.' : undefined,
    });
  }

  checks.push({
    ok: i.openTasks === 0,
    text: i.openTasks
      ? `${i.openTasks} open TODO${i.openTasks === 1 ? '' : 's'} still in the source`
      : 'No TODOs left in the source',
  });

  checks.push(
    i.buildProblems === null
      ? {
          ok: false,
          text: 'Not built yet',
          hint: 'Build to check for errors and count pages.',
        }
      : {
          ok: i.buildProblems === 0,
          text: i.buildProblems
            ? `Last build: ${i.buildProblems} error${i.buildProblems === 1 ? '' : 's'} or warning${i.buildProblems === 1 ? '' : 's'}`
            : 'Last build: no errors or warnings',
        },
  );

  const bib = /\\(bibliography|addbibresource|printbibliography)\b/.test(i.mainText + '\n' + i.otherTex.join('\n'));
  checks.push({
    ok: bib,
    text: bib ? 'Bibliography included' : 'No bibliography command found',
  });
  return checks;
}
