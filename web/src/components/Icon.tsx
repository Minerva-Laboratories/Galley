// Stroke icons from the prototype. Sized by the parent's CSS.
const PATHS: Record<string, string> = {
  files: 'M14 3v5h5M6 3h8l5 5v13H6z',
  outline: 'M4 6h16M4 12h10M4 18h13',
  history: 'M3 12a9 9 0 1 0 3-6.7M3 4v5h5M12 8v4l3 2',
  tasks: 'M9 6h11M9 12h11M9 18h11M4 6l1 1 2-2M4 12l1 1 2-2M4 18l1 1 2-2',
  submit: 'M12 19V5M5 12l7-7 7 7M5 21h14',
  bib: 'M4 19V5a2 2 0 0 1 2-2h13v16H6a2 2 0 0 0-2 2zm0 0a2 2 0 0 0 2 2h13M9 7h6M9 11h6',
  agents: 'M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8zM19 17l.7 2.3L22 20l-2.3.7L19 23l-.7-2.3L16 20l2.3-.7z',
  search: 'M18 18a7 7 0 1 0-14 0 7 7 0 0 0 14 0zM20 20l-3.5-3.5',
  theme: 'M12 3a9 9 0 1 0 9 9c0-.5 0-1-.1-1.4A5.5 5.5 0 0 1 12 3z',
  chevron: 'm6 9 6 6 6-6',
  file: 'M14 3v5h5M6 3h8l5 5v13H6z',
  plus: 'M12 5v14M5 12h14',
  close: 'M6 6l12 12M18 6 6 18',
  back: 'M15 6l-6 6 6 6',
  download: 'M12 4v11m-5-5 5 5 5-5M5 20h14',
  upload: 'M12 16V5m-5 5 5-5 5 5M5 20h14',
  comments: 'M21 12a8 8 0 0 1-8 8H8l-4 3V12a8 8 0 0 1 8-8h1a8 8 0 0 1 8 8z',
  share: 'M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1',
  check: 'M5 12l4 4 10-10',
  user: 'M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM4 21a8 8 0 0 1 16 0',
  eye: 'M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7zM12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z',
  'eye-off': 'M3 3l18 18M10.6 10.6a3 3 0 0 0 4.2 4.2M9.9 5.1A9.6 9.6 0 0 1 12 5c6.5 0 10 7 10 7a17 17 0 0 1-3 3.8M6.1 6.1A17 17 0 0 0 2 12s3.5 7 10 7a9.5 9.5 0 0 0 3-.5',
};

export function Icon({ name, size }: { name: keyof typeof PATHS | string; size?: number }) {
  const d = PATHS[name] ?? '';
  const s = size ? { width: size, height: size } : undefined;
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" style={s}>
      <path d={d} />
    </svg>
  );
}
