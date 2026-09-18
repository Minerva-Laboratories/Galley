// Comments and suggestions anchor to Yjs relative positions, so they follow the text as it moves
// and survive concurrent edits. The server stores the base64 blob opaquely.
import * as Y from 'yjs';

function toB64(bytes: Uint8Array): string {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

function fromB64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export function encodeAnchor(ytext: Y.Text, index: number): string {
  const rel = Y.createRelativePositionFromTypeIndex(ytext, index);
  return toB64(Y.encodeRelativePosition(rel));
}

/** Resolve an anchor to a current absolute index, or null if the text it referred to is gone. */
export function resolveAnchor(ydoc: Y.Doc, anchor: string): number | null {
  try {
    const rel = Y.decodeRelativePosition(fromB64(anchor));
    const abs = Y.createAbsolutePositionFromRelativePosition(rel, ydoc);
    return abs ? abs.index : null;
  } catch {
    return null;
  }
}
