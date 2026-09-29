import { useEffect, useLayoutEffect, useState } from 'preact/hooks';
import type { ComponentChildren } from 'preact';
import { finishTour } from '../store/store';

type Step = {
  title: string;
  body: ComponentChildren;
  /** A CSS selector for the part of the screen the step is about. Without one, or when the part is
   * not on screen, the card sits in the middle. */
  target?: string;
};

const STEPS: Step[] = [
  {
    title: 'Welcome to the Galley beta',
    body: (
      <>
        <p>Galley is a LaTeX editor for writing together. This tour takes about a minute and shows where everything is.</p>
        <p>
          <b>This is a beta.</b> Keep your own copy of anything you write here, whether that is a zip download, a copy in
          Overleaf, or a folder on your computer.
        </p>
      </>
    ),
  },
  {
    title: 'Write together',
    target: '.cm-editor',
    body: (
      <p>
        Everyone with access edits the same text live, with each person's cursor shown. Edits save as you type, even
        offline, and catch up when you reconnect.
      </p>
    ),
  },
  {
    title: 'Build the PDF',
    target: '[data-tour="build"]',
    body: (
      <p>
        Press <kbd>Ctrl</kbd> + <kbd>Enter</kbd> or Build. When something fails, the problem appears as a card that says
        what went wrong, where, and often offers a one-click fix.
      </p>
    ),
  },
  {
    title: 'Jump between source and PDF',
    target: '.preview',
    body: (
      <p>
        Click anywhere on the PDF to jump to that line of source. Click a line in the editor to find it in the PDF, which
        the Follow button above the PDF turns on and off.
      </p>
    ),
  },
  {
    title: 'History you never have to manage',
    target: '.rail button[aria-label="History"]',
    body: (
      <p>
        Every pause in typing is saved as a version. Compare any two, restore an old one, or mark a checkpoint such as
        "submitted". Nothing is ever overwritten.
      </p>
    ),
  },
  {
    title: 'A bibliography that helps',
    target: '.rail button[aria-label="Bibliography"]',
    body: (
      <p>
        Add a reference by DOI, by arXiv id, or by typing its title and picking the right match. Galley flags duplicates
        and missing fields, and the Graph button shows how your citations cluster.
      </p>
    ),
  },
  {
    title: 'Comments and sharing',
    target: '.rail button[aria-label="Comments"]',
    body: (
      <p>
        Select text to comment on it, or turn on Suggesting to propose edits that others accept or reject. Owners invite
        people and make links from Share, with a role for each: editor, commenter or viewer.
      </p>
    ),
  },
  {
    title: 'Connect your AI assistant',
    target: '[data-tour="connect"]',
    body: (
      <p>
        Connect AI lets Claude Code, Cursor or another assistant read the paper, compile it and propose edits, using your
        own account. Its edits arrive as suggestions, so nothing changes until someone accepts them.
      </p>
    ),
  },
  {
    title: 'Bring work in, take it out',
    target: '.rail button[aria-label="Files"]',
    body: (
      <p>
        From the projects page, Import zip brings an Overleaf project over: in Overleaf use Menu, then Download, then
        Source. Galley finds the main file by itself. Under the file list, Download the project as a zip gives you a copy
        at any time.
      </p>
    ),
  },
  {
    title: 'What the beta does not do yet',
    body: (
      <>
        <ul>
          <li>Push to or pull from GitHub and other git remotes.</li>
          <li>Import a library from Zotero.</li>
          <li>Sign in with a university account.</li>
          <li>Check every reference automatically for existence and retractions.</li>
          <li>Export to Word, or write in Typst.</li>
          <li>Edit comfortably on a phone. Reading and commenting work.</li>
        </ul>
        <p>
          These are planned. Tell the person who invited you what you miss most, and anything that breaks. Remember to
          keep your own copy of your work while this is a beta.
        </p>
      </>
    ),
  },
];

export function Tour() {
  const [i, setI] = useState(0);
  const [rect, setRect] = useState<DOMRect | null>(null);
  const step = STEPS[i] ?? STEPS[0]!;
  const last = i === STEPS.length - 1;

  // Find the part of the screen this step is about, and follow it if the window changes size.
  useLayoutEffect(() => {
    const measure = () => {
      const el = step.target ? document.querySelector<HTMLElement>(step.target) : null;
      const r = el?.getBoundingClientRect();
      setRect(r && r.width > 0 && r.height > 0 ? r : null);
    };
    measure();
    window.addEventListener('resize', measure);
    return () => window.removeEventListener('resize', measure);
  }, [i]);

  // Take focus away from the editor, so keys pressed during the tour never reach the document.
  useEffect(() => {
    (document.activeElement as HTMLElement | null)?.blur();
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') finishTour();
      else if (e.key === 'ArrowRight' || e.key === 'Enter') {
        if (last) finishTour();
        else setI(i + 1);
      }
      else if (e.key === 'ArrowLeft' && i > 0) setI(i - 1);
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    // Capture, so the tour sees the key before anything underneath it does.
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [i, last]);

  // Put the card beside the highlighted part when there is room, otherwise in the middle.
  const cardWidth = 380;
  let style: Record<string, string | number> = {};
  if (rect) {
    const below = rect.bottom + 12;
    const roomBelow = window.innerHeight - below > 260;
    const left = Math.min(Math.max(12, rect.left + rect.width / 2 - cardWidth / 2), window.innerWidth - cardWidth - 12);
    style = roomBelow
      ? { top: below, left }
      : { top: Math.max(12, rect.top - 12), left, transform: 'translateY(-100%)' };
    // A target taller than the window, such as the editor, gets the card over its middle.
    if (rect.height > window.innerHeight * 0.6) {
      style = { top: '50%', left: Math.min(rect.left + 40, window.innerWidth - cardWidth - 12), transform: 'translateY(-50%)' };
    }
  }

  return (
    <div class="tour" role="dialog" aria-label="Welcome tour" aria-live="polite">
      {rect ? (
        <div
          class="tour-ring"
          style={{ top: rect.top - 6, left: rect.left - 6, width: rect.width + 12, height: rect.height + 12 }}
        />
      ) : (
        <div class="tour-dim" />
      )}
      <div class={`tour-card ${rect ? '' : 'centered'}`} style={rect ? style : undefined}>
        <div class="tour-count">
          {i + 1} of {STEPS.length}
        </div>
        <h3>{step.title}</h3>
        <div class="tour-body">{step.body}</div>
        <div class="tour-actions">
          {!last && (
            <button class="tb" onClick={finishTour}>
              Skip tour
            </button>
          )}
          <span style={{ marginLeft: 'auto' }} />
          {i > 0 && (
            <button class="tb" onClick={() => setI(i - 1)}>
              Back
            </button>
          )}
          <button class="tb primary" onClick={() => (last ? finishTour() : setI(i + 1))}>
            {last ? 'Start writing' : 'Next'}
          </button>
        </div>
      </div>
    </div>
  );
}
