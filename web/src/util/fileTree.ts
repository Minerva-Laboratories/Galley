import type { FileEntry } from '../api';

export type FileTreeRow = { path: string; name: string; depth: number; file?: FileEntry };

/** Keep full paths for actions, but display basenames beneath their parent folders. */
export function fileTree(files: FileEntry[], folders: string[], collapsed: ReadonlySet<string>): FileTreeRow[] {
  const dirs = new Set<string>();
  const addParents = (path: string) => {
    const parts = path.split('/');
    for (let i = 1; i < parts.length; i++) dirs.add(parts.slice(0, i).join('/'));
  };
  for (const path of folders) { dirs.add(path); addParents(path); }
  for (const file of files) addParents(file.path);
  const children = new Map<string, FileTreeRow[]>();
  const add = (path: string, file?: FileEntry) => {
    const parts = path.split('/');
    const name = parts.pop()!;
    const parent = parts.join('/');
    const siblings = children.get(parent) ?? [];
    siblings.push({ path, name, depth: parts.length, file });
    children.set(parent, siblings);
  };
  for (const path of dirs) add(path);
  for (const file of files) add(file.path, file);
  const rows: FileTreeRow[] = [];
  const visit = (parent: string) => {
    const siblings = children.get(parent) ?? [];
    siblings.sort((a, b) => Number(!!a.file) - Number(!!b.file) || a.name.localeCompare(b.name));
    for (const row of siblings) {
      rows.push(row);
      if (!row.file && !collapsed.has(row.path)) visit(row.path);
    }
  };
  visit('');
  return rows;
}
