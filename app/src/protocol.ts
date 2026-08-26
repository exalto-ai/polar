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
  /**
   * A local editor update with an observed input source byte before the raw
   * Yjs update. The legacy Update tag remains valid and means Unknown, so a
   * newer daemon can accept windows from an older app without inventing
   * provenance for them.
   */
  SourcedUpdate: 0x09,
} as const;

export type Frame = { tag: number; docId: string; body: Uint8Array };

/**
 * How content entered the local editor. This is deliberately separate from
 * who wrote it: source is an observed interaction, while actor identity lives
 * in the daemon's provenance log.
 */
export const LocalInputSource = {
  Unknown: "unknown",
  Written: "written",
  Paste: "paste",
  Import: "import",
  Command: "command",
} as const;

export type LocalInputSource =
  (typeof LocalInputSource)[keyof typeof LocalInputSource];

const SOURCE_CODE: Record<LocalInputSource, number> = {
  [LocalInputSource.Unknown]: 0x00,
  [LocalInputSource.Written]: 0x01,
  [LocalInputSource.Paste]: 0x02,
  [LocalInputSource.Import]: 0x03,
  [LocalInputSource.Command]: 0x04,
};

const SOURCE_BY_CODE = new Map<number, LocalInputSource>(
  Object.entries(SOURCE_CODE).map(([source, code]) => [code, source as LocalInputSource]),
);

export type SourcedUpdate = {
  source: LocalInputSource;
  update: Uint8Array;
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

/** Encode source and update as one indivisible, positional-ACK queue entry. */
export function encodeSourcedUpdate(
  docId: string,
  source: LocalInputSource,
  update: Uint8Array,
): Uint8Array {
  const body = new Uint8Array(update.length + 1);
  body[0] = SOURCE_CODE[source];
  body.set(update, 1);
  return encode(Tag.SourcedUpdate, docId, body);
}

/** Decode the body only after the outer frame has identified the new tag. */
export function decodeSourcedUpdate(frame: Frame): SourcedUpdate | null {
  if (frame.tag !== Tag.SourcedUpdate || frame.body.length < 1) return null;
  const source = SOURCE_BY_CODE.get(frame.body[0]);
  if (!source) return null;
  return { source, update: frame.body.subarray(1) };
}
