import { useEffect, useRef, useState } from 'preact/hooks';

/** A name that turns into a text field when clicked, wherever a project's name is shown. Enter or
 * leaving the field saves, Escape cancels. The click never reaches the parent, so clicking the name
 * on a project card renames it rather than opening it. */
export function EditableName({
  value,
  canEdit,
  onSave,
  class: className = '',
  maxLength = 120,
}: {
  value: string;
  canEdit: boolean;
  onSave: (name: string) => Promise<boolean> | boolean;
  class?: string;
  maxLength?: number;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const input = useRef<HTMLInputElement>(null);
  // Removing the field fires blur, and blur saves. After Enter or Escape has already settled the edit,
  // that late blur must do nothing, or Escape would save the text it was meant to throw away.
  const settled = useRef(false);

  useEffect(() => {
    if (!editing) setDraft(value);
  }, [value, editing]);

  if (!canEdit) return <span class={className}>{value}</span>;

  const start = () => {
    settled.current = false;
    setDraft(value);
    setEditing(true);
  };

  const commit = async () => {
    if (settled.current) return;
    settled.current = true;
    const next = draft.trim();
    setEditing(false);
    if (!next || next === value) {
      setDraft(value);
      return;
    }
    if (!(await onSave(next))) setDraft(value);
  };

  if (editing) {
    return (
      <input
        ref={(el) => {
          input.current = el;
          // Focus as the field mounts, not an effect later, so the first keystroke already lands in it.
          if (el && document.activeElement !== el) {
            el.focus();
            el.select();
          }
        }}
        class={`name-edit ${className}`}
        value={draft}
        maxLength={maxLength}
        aria-label="Project name"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
        onInput={(e) => setDraft((e.target as HTMLInputElement).value)}
        onBlur={() => void commit()}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === 'Enter') {
            e.preventDefault();
            void commit();
          } else if (e.key === 'Escape') {
            settled.current = true;
            setDraft(value);
            setEditing(false);
          }
        }}
      />
    );
  }

  return (
    <span
      class={`name-editable ${className}`}
      role="button"
      tabIndex={0}
      title="Click to rename"
      onClick={(e) => {
        e.stopPropagation();
        e.preventDefault();
        start();
      }}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === 'F2') {
          e.preventDefault();
          e.stopPropagation();
          start();
        }
      }}
    >
      {value}
    </span>
  );
}
