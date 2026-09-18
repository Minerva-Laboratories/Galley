import { closeTab, currentFile, openTabs } from '../store/store';

export function Tabs() {
  return (
    <div class="tabs" role="tablist">
      {openTabs.value.map((path) => (
        <div
          key={path}
          class={`tab ${path === currentFile.value ? 'on' : ''}`}
          role="tab"
          tabIndex={0}
          aria-selected={path === currentFile.value}
          onClick={() => (currentFile.value = path)}
          onKeyDown={(e) => e.key === 'Enter' && (currentFile.value = path)}
        >
          {path}
          {openTabs.value.length > 1 && (
            <button
              class="x"
              title="Close"
              aria-label={`Close ${path}`}
              onClick={(e) => {
                e.stopPropagation();
                closeTab(path);
              }}
            >
              ×
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
