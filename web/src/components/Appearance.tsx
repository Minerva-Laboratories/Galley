import { useEffect, useRef, useState } from 'preact/hooks';
import { accentColor, setAccentColor, setTheme, theme } from '../store/store';
import { ACCENT_PRESETS, DEFAULT_ACCENT, normalizeHex, onAccent } from '../util/accent';
import { Icon } from './Icon';

export function AppearanceButton() {
  const dialog = useRef<HTMLDialogElement>(null);
  const [hex, setHex] = useState(accentColor.value.toUpperCase());
  const [invalid, setInvalid] = useState(false);
  const color = accentColor.value;
  const selected = ACCENT_PRESETS.find((preset) => preset.color === color)?.name ?? 'Custom';
  // Each control here updates the hex field at once. The effect below runs after paint, which can be
  // after the person has typed something new, so it only follows changes made elsewhere, such as in
  // another tab, and never repeats a change this dialog made.
  const own = useRef(color);
  useEffect(() => {
    if (color === own.current) return;
    own.current = color;
    setHex(color.toUpperCase());
    setInvalid(false);
  }, [color]);
  const pick = (next: string) => {
    own.current = next;
    setAccentColor(next);
    setHex(next.toUpperCase());
    setInvalid(false);
  };
  const applyHex = () => {
    const normalized = normalizeHex(hex);
    setInvalid(!normalized);
    if (normalized) { own.current = normalized; setAccentColor(normalized); setHex(normalized.toUpperCase()); }
  };
  return (
    <>
      <button class="tb icon" aria-label="Appearance" title="Appearance" onClick={() => {
        setHex(color.toUpperCase()); setInvalid(false); dialog.current?.showModal();
      }}>
        <Icon name="palette" size={16} />
      </button>
      <dialog class="appearance-dialog" ref={dialog} aria-label="Appearance" onClick={(e) => {
        if (e.target === e.currentTarget) {
          const rect = e.currentTarget.getBoundingClientRect();
          if (e.clientX < rect.left || e.clientX > rect.right || e.clientY < rect.top || e.clientY > rect.bottom) e.currentTarget.close();
        }
      }}>
        <header class="appearance-heading">
          <div><h2>Appearance</h2><p>A workspace that feels like yours.</p></div>
          <button class="tb icon" aria-label="Close appearance" onClick={() => dialog.current?.close()}><Icon name="close" size={16} /></button>
        </header>
        <div class="appearance-body">
          <div class="appearance-preview" aria-hidden="true">
            <div class="appearance-preview-top"><span class="wordmark">galley<span class="caret">^</span></span><span class="appearance-preview-tag">Your workspace</span></div>
            <div class="appearance-preview-tabs"><span>main.tex</span><span>references.bib</span></div>
            <div class="appearance-preview-content"><span class="appearance-preview-lines"><i /><i /><i /></span><span class="appearance-preview-action">Build <Icon name="check" size={13} /></span></div>
          </div>
          <div class="appearance-section-label"><label>Theme</label></div>
          <div class="appearance-themes" role="group" aria-label="Theme">
            {(['light', 'dark'] as const).map((value) => (
              <button class={theme.value === value ? 'selected' : ''} aria-pressed={theme.value === value} onClick={() => setTheme(value)}>
                <Icon name={value === 'light' ? 'sun' : 'theme'} size={16} /> {value === 'light' ? 'Light' : 'Dark'}
              </button>
            ))}
          </div>
          <div class="appearance-section-label"><label>Accent color</label><span>{selected}</span></div>
          <div class="accent-presets" role="group" aria-label="Accent colors">
            {ACCENT_PRESETS.map((preset) => (
              <button class="accent-swatch" aria-label={`${preset.name} accent`} title={preset.name} aria-pressed={color === preset.color}
                style={{ background: preset.color, color: onAccent(preset.color) }} onClick={() => pick(preset.color)}>
                {color === preset.color && <Icon name="check" size={17} />}
              </button>
            ))}
          </div>
          <div class="accent-custom">
            <label class="accent-picker" style={{ background: color, color: onAccent(color) }} title="Choose a custom color">
              <Icon name="palette" size={20} />
              <input type="color" aria-label="Custom accent color" value={color} onInput={(e) => pick(e.currentTarget.value)} />
            </label>
            <div class="accent-custom-label"><b>Custom color</b><span>Pick any shade you like</span></div>
            <input class="accent-hex" aria-label="Accent hex color" aria-invalid={invalid} aria-describedby={invalid ? 'accent-error' : undefined}
              spellcheck={false} maxLength={7} value={hex} onInput={(e) => { setHex(e.currentTarget.value); setInvalid(false); }}
              onBlur={applyHex} onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); applyHex(); } }} />
          </div>
          {invalid && <p id="accent-error" class="err" role="alert">Enter a hex color, such as #287BCC.</p>}
          <p class="appearance-note">Applied instantly. Saved in this browser.</p>
        </div>
        <footer class="appearance-footer">
          <button class="tb" onClick={() => pick(DEFAULT_ACCENT)}>Reset accent</button>
          <button class="tb primary" onClick={() => dialog.current?.close()}>Done</button>
        </footer>
      </dialog>
    </>
  );
}
