/**
 * The sync wire format, mirroring `crates/thoughtd/src/sync.rs`.
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
  /**
   * An agent wrote. Agents connect over MCP, which has no awareness protocol,
   * so presence is inferred from edits rather than pretended into the awareness
   * channel, a different kind of signal that must not be conflated with it.
   */
  Presence: 0x07,
  /**
   * The daemon durably committed one Update frame. Acknowledgements are
   * ordered with updates on the WebSocket, so the provider can keep a FIFO of
   * edits that still need to reach disk without putting sequence numbers in
   * every keystroke.
   */
  Ack: 0x08,
  /** One complete local editor dispatch with ProseMirror before/after ranges. */
  EditorMutation: 0x09,
} as const;

export type Frame = { tag: number; docId: string; body: Uint8Array };

export const MAX_EDITOR_RANGES = 64;

export type EditorRange = {
  beforeFrom: number;
  beforeTo: number;
  afterFrom: number;
  afterTo: number;
};

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

export function encodeEditorMutation(
  docId: string,
  ranges: readonly EditorRange[],
  update: Uint8Array,
): Uint8Array {
  if (ranges.length > MAX_EDITOR_RANGES) {
    throw new RangeError(`an editor mutation may have at most ${MAX_EDITOR_RANGES} ranges`);
  }
  if (update.length === 0) throw new RangeError("an editor mutation needs an update");

  const body = new Uint8Array(1 + ranges.length * 16 + update.length);
  const view = new DataView(body.buffer);
  body[0] = ranges.length;
  let offset = 1;
  for (const range of ranges) {
    const values = [range.beforeFrom, range.beforeTo, range.afterFrom, range.afterTo];
    if (
      values.some(
        (value) => !Number.isInteger(value) || value < 0 || value > 0xffff_ffff,
      ) ||
      range.beforeFrom > range.beforeTo ||
      range.afterFrom > range.afterTo
    ) {
      throw new RangeError("editor ranges must contain ordered u32 positions");
    }
    for (const value of values) {
      view.setUint32(offset, value, false);
      offset += 4;
    }
  }
  body.set(update, offset);
  return encode(Tag.EditorMutation, docId, body);
}
