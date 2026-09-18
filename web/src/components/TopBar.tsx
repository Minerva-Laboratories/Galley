import { useEffect, useRef, useState } from 'preact/hooks';
import { logout } from '../store/auth';
import { daysLeft, setDeadline } from '../store/settings';
import { colorFor, initials } from '../sync/colors';
import { setUserName } from '../sync/docs';
import {
  canEdit,
  connection,
  currentUser,
  displayName,
  isAdmin,
  navigate,
  paletteOpen,
  peers,
  project,
  quietMode,
  setDisplayName,
  shareOpen,
  theme,
  toggleQuietMode,
  toggleTheme,
} from '../store/store';
import { Icon } from './Icon';

export function TopBar() {
  const [editing, setEditing] = useState(displayName.value === '');
  const me = displayName.value || 'Anonymous';
  const others = peers.value;
  const online = others.length + 1;

  return (
    <header class="top">
      <a class="wordmark" href="/" aria-label="Galley" onClick={(e) => (e.preventDefault(), navigate('/'))}>
        galley<span class="caret">^</span>
      </a>
      <button class="proj" title="Switch project" onClick={() => navigate('/')}>
        {project.value?.name ?? '…'} <Icon name="chevron" size={12} />
      </button>
      <button class="tb search" onClick={() => (paletteOpen.value = true)}>
        <Icon name="search" size={14} />
        <span>Search files, commands, sections</span>
        <kbd>Ctrl K</kbd>
      </button>
      <DeadlinePill />
      <div class="presence">
        {connection.value === 'offline' && <span class="pill off">Offline · edits stored locally</span>}
        {connection.value === 'connecting' && <span class="pill wait">Connecting…</span>}
        <div class="avs" title={[...others.map((p) => p.name), `${me} (you)`].join(', ')}>
          {others.slice(0, 4).map((p) => (
            <span key={p.clientId} class="av" style={{ background: p.color }}>
              {initials(p.name)}
            </span>
          ))}
          <button class="av me" title="Change your display name" onClick={() => setEditing(true)}>
            {initials(me)}
          </button>
        </div>
        <span class="n">{online === 1 ? 'Only you' : `${online} online`}</span>
        {editing && <NamePopover onClose={() => setEditing(false)} />}
      </div>
      <button
        class={`tb icon ${quietMode.value ? 'on' : ''}`}
        title={quietMode.value ? 'Quiet mode on: collaborator cursors hidden' : 'Quiet mode: hide collaborator cursors'}
        aria-label="Toggle quiet mode"
        aria-pressed={quietMode.value}
        onClick={toggleQuietMode}
      >
        <Icon name={quietMode.value ? 'eye-off' : 'eye'} size={16} />
      </button>
      {isAdmin.value && (
        <button class="tb" onClick={() => (shareOpen.value = true)}>
          <Icon name="share" size={14} /> Share
        </button>
      )}
      <button
        class="tb icon"
        title={theme.value === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
        aria-label="Toggle theme"
        onClick={toggleTheme}
      >
        <Icon name="theme" size={16} />
      </button>
      <UserMenu />
    </header>
  );
}

function UserMenu() {
  const [open, setOpen] = useState(false);
  const user = currentUser.value;
  useEffect(() => {
    if (!open) return;
    const close = () => setOpen(false);
    window.addEventListener('click', close);
    return () => window.removeEventListener('click', close);
  }, [open]);
  return (
    <div style={{ position: 'relative' }}>
      <button class="tb icon" title="Account" aria-label="Account" onClick={(e) => (e.stopPropagation(), setOpen(!open))}>
        <Icon name="user" size={16} />
      </button>
      {open && (
        <div class="name-pop" style={{ width: 220 }} onClick={(e) => e.stopPropagation()}>
          <div style={{ fontWeight: 500 }}>{user?.name}</div>
          <div class="hint" style={{ padding: '2px 0 8px' }}>{user?.is_guest ? 'Guest (via share link)' : user?.email}</div>
          <button class="tb" style={{ width: '100%', justifyContent: 'center' }} onClick={() => void logout().then(() => navigate('/'))}>
            Sign out
          </button>
        </div>
      )}
    </div>
  );
}

function NamePopover({ onClose }: { onClose: () => void }) {
  const input = useRef<HTMLInputElement>(null);
  const [value, setValue] = useState(displayName.value);
  useEffect(() => input.current?.focus(), []);

  const save = () => {
    const name = value.trim();
    if (!name) return;
    setDisplayName(name);
    setUserName(name);
    onClose();
  };

  return (
    <form
      class="name-pop"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <label for="displayName">Your name, as collaborators see it</label>
      <div class="inline-form" style={{ padding: 0 }}>
        <input
          id="displayName"
          ref={input}
          value={value}
          maxLength={40}
          placeholder="Ana Novak"
          style={{ fontFamily: 'var(--font)' }}
          onInput={(e) => setValue((e.target as HTMLInputElement).value)}
          onKeyDown={(e) => e.key === 'Escape' && displayName.value && onClose()}
        />
        <button class="tb primary" type="submit" disabled={!value.trim()}>
          Save
        </button>
      </div>
      <div class="hint">
        Shown on your cursor in <span style={{ color: colorFor(value || 'anonymous').color }}>this colour</span> and
        on the commits you make.
      </div>
    </form>
  );
}

/** The prototype's deadline pill: amber at 7 days, red at 3. Editors can set one from here. */
function DeadlinePill() {
  const meta = project.value;
  if (!meta) return null;
  const days = daysLeft(meta.deadline);
  if (days === null) {
    if (!canEdit.value) return null;
    return (
      <button class="dl" title="Set a deadline" onClick={() => void setDeadline()}>
        Set deadline
      </button>
    );
  }
  const label = meta.venue || 'Deadline';
  const when = days < 0 ? 'past due' : days === 0 ? 'today' : `${days} day${days === 1 ? '' : 's'}`;
  return (
    <button
      class={`dl ${days <= 3 ? 'now' : days <= 7 ? 'soon' : ''}`}
      title={canEdit.value ? 'Click to change the deadline' : (meta.deadline ?? '')}
      onClick={() => canEdit.value && void setDeadline()}
    >
      {label} · {when}
    </button>
  );
}
