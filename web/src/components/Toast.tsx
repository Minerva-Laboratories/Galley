import { toast } from '../store/store';

export function Toast() {
  const t = toast.value;
  if (!t) return null;
  return (
    <div class="toast" role="status" aria-live="polite">
      {t.message}
      {t.action && (
        <button
          onClick={() => {
            t.action?.run();
            toast.value = null;
          }}
        >
          {t.action.label}
        </button>
      )}
    </div>
  );
}
