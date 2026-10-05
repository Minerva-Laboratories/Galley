import { Icon } from './Icon';

const TYPES: { extensions: string[]; icon: string; label: string }[] = [
  { extensions: ['tex', 'ltx', 'latex'], icon: 'file-tex', label: 'LaTeX document' },
  { extensions: ['bib'], icon: 'bib', label: 'Bibliography' },
  { extensions: ['pdf'], icon: 'file-pdf', label: 'PDF document' },
  { extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'eps', 'ps', 'tif', 'tiff', 'bmp', 'ico', 'avif'], icon: 'image', label: 'Image' },
  { extensions: ['cls', 'sty', 'bst', 'bbx', 'cbx', 'lbx', 'cfg', 'def'], icon: 'file-settings', label: 'LaTeX style or configuration' },
  { extensions: ['csv', 'tsv', 'dat', 'xls', 'xlsx', 'ods'], icon: 'table', label: 'Data or spreadsheet' },
  { extensions: ['zip', 'gz', 'tar', 'tgz', '7z', 'rar', 'bz2', 'xz'], icon: 'archive', label: 'Archive' },
  { extensions: ['py', 'r', 'lua', 'js', 'ts', 'sh', 'json', 'yaml', 'yml', 'toml', 'xml', 'html', 'css'], icon: 'code', label: 'Code or configuration' },
  { extensions: ['txt', 'md', 'rst', 'log', 'rtf', 'doc', 'docx', 'odt'], icon: 'file-text', label: 'Text document' },
];

/** Classify the basename only: a folder's extension must not affect its files. */
export function FileIcon({ path }: { path: string }) {
  const name = path.split('/').pop() ?? path;
  const extension = name.includes('.') ? name.split('.').pop()!.toLowerCase() : '';
  const type = TYPES.find((type) => type.extensions.includes(extension));
  const label = type?.label ?? 'File';
  return (
    <span class="file-kind-icon" role="img" aria-label={label} title={label}>
      <Icon name={type?.icon ?? 'file'} />
    </span>
  );
}
