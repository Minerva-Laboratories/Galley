import { useEffect, useRef, useState } from 'preact/hooks';
import { api, ApiError, type FileEntry } from '../api';
import { saveSettings } from '../store/settings';
import { canEdit, currentFile, files, forgetFile, movedFile, openFile, project, showToast } from '../store/store';
import { closeDoc } from '../sync/docs';
import { Icon } from './Icon';

/** Formats the browser can show on its own. Everything else downloads. */
const VIEWABLE = /\.(pdf|png|jpe?g|gif|webp)$/i;
/** Files a drag from a desktop often carries along and nobody means to upload. */
const JUNK = /(^|\/)(\.DS_Store|Thumbs\.db|desktop\.ini)$|(^|\/)(\.git|__MACOSX)\//;

type Upload = { path: string; file: File };

function message(e: unknown, fallback: string): string {
  return e instanceof ApiError ? e.message : fallback;
}

async function refreshFiles(id: string) {
  try {
    files.value = await api.listFiles(id);
  } catch {
    // The next event or a reload brings the list back in line.
  }
}

/** Upload files one by one, asking before replacing any that already exist. */
async function uploadAll(items: Upload[]) {
  const id = project.value?.id;
  if (!id || items.length === 0) return;
  let done = 0;
  const failed: string[] = [];
  for (const { path, file } of items) {
    if (JUNK.test(path)) continue;
    try {
      await api.uploadFile(id, path, file);
      done++;
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        if (!window.confirm(`${path} already exists. Replace it? The old version stays in History.`)) continue;
        try {
          await api.uploadFile(id, path, file, true);
          done++;
        } catch (e2) {
          failed.push(`${path}: ${message(e2, 'upload failed')}`);
        }
      } else if (e instanceof ApiError && e.status === 413) {
        failed.push(`${path}: larger than 25 MB`);
      } else {
        failed.push(`${path}: ${message(e, 'upload failed')}`);
      }
    }
  }
  await refreshFiles(id);
  if (failed.length) showToast(`Uploaded ${done}. Not uploaded: ${failed.join('; ')}`);
  else if (done) showToast(`Uploaded ${done} file${done === 1 ? '' : 's'}.`);
}

/** Walk a dropped folder, keeping its structure. */
async function readEntry(entry: FileSystemEntry, prefix: string, out: Upload[]) {
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) => (entry as FileSystemFileEntry).file(resolve, reject));
    out.push({ path: prefix + entry.name, file });
    return;
  }
  if (!entry.isDirectory) return;
  const reader = (entry as FileSystemDirectoryEntry).createReader();
  // readEntries returns results in batches until it returns an empty one.
  for (;;) {
    const batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject));
    if (batch.length === 0) break;
    for (const child of batch) await readEntry(child, `${prefix}${entry.name}/`, out);
  }
}

async function droppedFiles(dt: DataTransfer): Promise<Upload[]> {
  const out: Upload[] = [];
  // Entries must be taken synchronously, before the first await, or the browser drops them.
  const entries = Array.from(dt.items)
    .map((i) => (i.kind === 'file' ? i.webkitGetAsEntry?.() : null))
    .filter((e): e is FileSystemEntry => !!e);
  const plain = Array.from(dt.files);
  if (entries.length) {
    for (const e of entries) await readEntry(e, '', out);
  } else {
    for (const f of plain) out.push({ path: f.name, file: f });
  }
  return out;
}

function download(id: string, path: string) {
  const a = document.createElement('a');
  a.href = api.fileUrl(id, path);
  a.download = path.split('/').pop() ?? path;
  document.body.appendChild(a);
  a.click();
  a.remove();
}

function sizeLabel(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export function FilesDrawer() {
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const picker = useRef<HTMLInputElement>(null);
  const depth = useRef(0);
  useEffect(() => {
    if (adding) input.current?.focus();
  }, [adding]);
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && setMenu(null);
    window.addEventListener('click', close);
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('keydown', onKey);
    };
  }, [menu]);

  const id = project.value?.id;
  const main = project.value?.main_file;
  const editable = canEdit.value;

  const create = async () => {
    const path = name.trim();
    if (!id || !path) return;
    try {
      const entry = await api.createFile(id, path);
      // The file_created event may have already added it. Dedupe by path either way.
      files.value = [...files.value.filter((f) => f.path !== entry.path), entry].sort((a, b) => a.path.localeCompare(b.path));
      setAdding(false);
      setName('');
      setError(null);
      openFile(entry.path);
      // A standalone .tex file is not part of the document until the main file pulls it in.
      if (entry.path.endsWith('.tex') && entry.path !== main) {
        const base = entry.path.replace(/\.tex$/, '');
        showToast(`Created ${entry.path}. Add \\input{${base}} to ${main ?? 'main.tex'} to include it in the build.`);
      }
    } catch (e) {
      setError(message(e, 'Could not create the file.'));
    }
  };

  const open = (f: FileEntry) => {
    if (!id) return;
    if (f.kind === 'text') openFile(f.path);
    else if (VIEWABLE.test(f.path)) window.open(api.fileUrl(id, f.path, true), '_blank', 'noopener');
    else download(id, f.path);
  };

  const rename = async (f: FileEntry) => {
    if (!id) return;
    const to = window.prompt(`Rename or move ${f.path}`, f.path)?.trim();
    if (!to || to === f.path) return;
    try {
      const r = await api.renameFile(id, f.path, to);
      movedFile(f.path, r.path);
      closeDoc(f.path);
      const note = f.path.endsWith('.tex') && f.path !== main ? ' Update any \\input that names the old path.' : '';
      showToast(`Renamed to ${r.path}.${note}`);
    } catch (e) {
      showToast(message(e, 'Could not rename the file.'));
    }
  };

  const remove = async (f: FileEntry) => {
    if (!id) return;
    if (!window.confirm(`Delete ${f.path}? You can bring it back from History.`)) return;
    try {
      await api.deleteFile(id, f.path);
      forgetFile(f.path);
      closeDoc(f.path);
      showToast(`Deleted ${f.path}. Restore it from History if you need it back.`);
    } catch (e) {
      showToast(message(e, 'Could not delete the file.'));
    }
  };

  const makeMain = async (f: FileEntry) => {
    if (await saveSettings({ main_file: f.path })) showToast(`${f.path} is now the main file. Builds compile it.`);
  };

  const drop = (e: DragEvent) => {
    e.preventDefault();
    depth.current = 0;
    setDragging(false);
    if (!editable || !e.dataTransfer) return;
    void droppedFiles(e.dataTransfer).then(uploadAll);
  };

  const hasFiles = (e: DragEvent) => !!e.dataTransfer && Array.from(e.dataTransfer.types).includes('Files');

  const list = files.value;
  return (
    <>
      <div class="dh">
        <span>Files</span>
        {editable && (
          <span style={{ display: 'flex', gap: 6 }}>
            <button class="tb" onClick={() => picker.current?.click()} title="Upload files, or drop them on the list">
              <Icon name="upload" size={13} /> Upload
            </button>
            <button class="tb" onClick={() => setAdding(true)}>
              <Icon name="plus" size={13} /> New file
            </button>
          </span>
        )}
        <input
          ref={picker}
          type="file"
          multiple
          hidden
          onChange={(e) => {
            const el = e.target as HTMLInputElement;
            const picked = Array.from(el.files ?? []).map((f) => ({ path: f.name, file: f }));
            el.value = '';
            void uploadAll(picked);
          }}
        />
      </div>
      <div
        class={`db files-db ${dragging ? 'dragging' : ''}`}
        onDragEnter={(e) => {
          if (!editable || !hasFiles(e)) return;
          e.preventDefault();
          depth.current++;
          setDragging(true);
        }}
        onDragOver={(e) => {
          if (editable && hasFiles(e)) e.preventDefault();
        }}
        onDragLeave={() => {
          depth.current = Math.max(0, depth.current - 1);
          if (depth.current === 0) setDragging(false);
        }}
        onDrop={drop}
      >
        {adding && (
          <form
            class="inline-form"
            onSubmit={(e) => {
              e.preventDefault();
              void create();
            }}
          >
            <input
              ref={input}
              placeholder="sections/method.tex"
              value={name}
              onInput={(e) => setName((e.target as HTMLInputElement).value)}
              onKeyDown={(e) => e.key === 'Escape' && (setAdding(false), setError(null))}
              aria-label="New file path"
            />
            <button class="tb primary" type="submit" disabled={!name.trim()}>
              Add
            </button>
          </form>
        )}
        {error && <div class="err">{error}</div>}
        {list.length === 0 && (
          <div class="empty">
            <b>No files yet</b>
            Add a .tex file to start writing, or drop files here.
          </div>
        )}
        {list.map((f) => (
          <div key={f.path} class={`row filerow ${f.path === currentFile.value ? 'on' : ''}`}>
            <button class="fname" onClick={() => open(f)} title={f.kind === 'text' ? `Open ${f.path}` : `${f.path} · ${sizeLabel(f.size)}`}>
              <Icon name="file" />
              <span class="n">{f.path}</span>
              <span class="m">{f.path === main ? 'main' : f.kind === 'binary' ? sizeLabel(f.size) : ''}</span>
            </button>
            <button
              class="more"
              aria-label={`Actions for ${f.path}`}
              aria-haspopup="menu"
              aria-expanded={menu === f.path}
              onClick={(e) => {
                e.stopPropagation();
                setMenu(menu === f.path ? null : f.path);
              }}
            >
              ⋯
            </button>
            {menu === f.path && id && (
              <div class="filemenu" role="menu" onClick={(e) => e.stopPropagation()}>
                <button role="menuitem" onClick={() => (setMenu(null), open(f))}>
                  {f.kind !== 'text' && VIEWABLE.test(f.path) ? 'View' : 'Open'}
                </button>
                <button role="menuitem" onClick={() => (setMenu(null), download(id, f.path))}>
                  Download
                </button>
                {editable && (
                  <button role="menuitem" onClick={() => (setMenu(null), void rename(f))}>
                    Rename or move…
                  </button>
                )}
                {editable && f.path.endsWith('.tex') && f.path !== main && (
                  <button role="menuitem" onClick={() => (setMenu(null), void makeMain(f))}>
                    Set as main file
                  </button>
                )}
                {editable && (
                  <button
                    role="menuitem"
                    class="danger"
                    disabled={f.path === main}
                    title={f.path === main ? 'Set another .tex file as the main file first' : undefined}
                    onClick={() => (setMenu(null), void remove(f))}
                  >
                    Delete
                  </button>
                )}
              </div>
            )}
          </div>
        ))}
        {editable && list.length > 0 && <div class="hint">Drop files or folders here to upload them, up to 25 MB each.</div>}
        {dragging && <div class="dropzone">Drop to upload</div>}
      </div>
    </>
  );
}
