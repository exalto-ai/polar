/**
 * The sync wire format, mirroring `crates/polard/src/sync.rs`.
 *
 * Length-prefixed binary: Yjs updates are binary, and base64 inside JSON would
 * inflate every keystroke.
 */
export const Tag = {
  Subscribe: 0x01,
  Sync: 0x02,
  Update: 0x03,
  Broadcast: 0x04,
  Awareness: 0x05,
  Error: 0x06,
} as const;

export type Frame = { tag: number; docId: string; body: Uint8Array };

export function encode(tag: number, docId: string, body: Uint8Array): Uint8Array {
  const id = new TextEncoder().encode(docId);
  const out = new Uint8Array(5 + id.length + body.length);
  out[0] = tag;
  new DataView(out.buffer).setUint32(1, id.length, false);
  out.set(id, 5);
  out.set(body, 5 + id.length);
  return out;
}

export function decode(bytes: Uint8Array): Frame | null {
  if (bytes.length < 5) return null;
  const idLength = new DataView(bytes.buffer, bytes.byteOffset).getUint32(1, false);
  // Guard before slicing: a truncated frame must not throw inside the socket
  // handler and take the connection down.
  if (bytes.length < 5 + idLength) return null;
  return {
    tag: bytes[0],
    docId: new TextDecoder().decode(bytes.subarray(5, 5 + idLength)),
    body: bytes.subarray(5 + idLength),
  };
}
