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
   * The daemon durably committed one immutable batch. Acknowledgements are
   * ordered on the WebSocket, so the provider can keep a FIFO of edits that
   * still need to reach disk without putting sequence numbers in every
   * keystroke.
   */
  Ack: 0x08,
  /**
   * A local editor update with an observed input source byte before the raw
   * Yjs update. The legacy Update tag remains valid and means Unknown, so a
   * newer daemon can accept windows from an older app without inventing
   * provenance for them.
   */
  SourcedUpdate: 0x09,
  /**
   * One or more editor dispatches, each carrying the ProseMirror ranges
   * observed before and after that complete dispatch. The batch is only a transport
   * optimization: mutation ordering and semantic boundaries remain explicit.
   */
  AnchoredBatch: 0x0a,
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

export const ANCHORED_BATCH_VERSION = 1 as const;
export const MAX_ANCHORED_MUTATIONS = 128;
export const MAX_ANCHORED_HINTS = 64;
export const MAX_CLIENT_EVENT_ID_BYTES = 64;

/** A ProseMirror change range on both sides of one complete editor dispatch. */
export type AnchoredRangeHint = {
  beforeFrom: number;
  beforeTo: number;
  afterFrom: number;
  afterTo: number;
};

/** One semantic editor dispatch and the Yjs update it emitted. */
export type AnchoredMutation = {
  source: LocalInputSource;
  clientEventId: string;
  hints: readonly AnchoredRangeHint[];
  update: Uint8Array;
};

export type AnchoredBatch = {
  version: typeof ANCHORED_BATCH_VERSION;
  mutations: AnchoredMutation[];
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

const textEncoder = new TextEncoder();

function isU32(value: number): boolean {
  return Number.isInteger(value) && value >= 0 && value <= 0xffff_ffff;
}

function encodedMutationLength(mutation: AnchoredMutation): number {
  const source = SOURCE_CODE[mutation.source];
  if (source === undefined) throw new RangeError("unknown local input source");

  const id = textEncoder.encode(mutation.clientEventId);
  if (id.length < 1 || id.length > MAX_CLIENT_EVENT_ID_BYTES) {
    throw new RangeError(
      `client event ID must be 1..${MAX_CLIENT_EVENT_ID_BYTES} UTF-8 bytes`,
    );
  }
  if (mutation.hints.length > MAX_ANCHORED_HINTS) {
    throw new RangeError(`an anchored mutation may have at most ${MAX_ANCHORED_HINTS} hints`);
  }
  for (const hint of mutation.hints) {
    if (
      !isU32(hint.beforeFrom) ||
      !isU32(hint.beforeTo) ||
      !isU32(hint.afterFrom) ||
      !isU32(hint.afterTo) ||
      hint.beforeFrom > hint.beforeTo ||
      hint.afterFrom > hint.afterTo
    ) {
      throw new RangeError("anchored hint positions must be ordered u32 values");
    }
  }
  if (mutation.update.length < 1 || mutation.update.length > 0xffff_ffff) {
    throw new RangeError("anchored mutation update must be 1..u32::MAX bytes");
  }

  return 1 + 1 + id.length + 2 + mutation.hints.length * 16 + 4 + mutation.update.length;
}

/**
 * Encode an ordered group of semantic editor dispatches. The function
 * validates before allocating so invalid metadata can never produce a frame
 * that the daemon would interpret differently.
 */
export function encodeAnchoredBatch(
  docId: string,
  mutations: readonly AnchoredMutation[],
): Uint8Array {
  if (mutations.length < 1 || mutations.length > MAX_ANCHORED_MUTATIONS) {
    throw new RangeError(`an anchored batch must contain 1..${MAX_ANCHORED_MUTATIONS} mutations`);
  }

  let bodyLength = 3;
  const ids: Uint8Array[] = [];
  for (const mutation of mutations) {
    bodyLength += encodedMutationLength(mutation);
    if (!Number.isSafeInteger(bodyLength) || bodyLength > 0xffff_ffff) {
      throw new RangeError("anchored batch body is too large");
    }
    ids.push(textEncoder.encode(mutation.clientEventId));
  }

  const body = new Uint8Array(bodyLength);
  const view = new DataView(body.buffer);
  let offset = 0;
  body[offset++] = ANCHORED_BATCH_VERSION;
  view.setUint16(offset, mutations.length, false);
  offset += 2;

  mutations.forEach((mutation, index) => {
    body[offset++] = SOURCE_CODE[mutation.source];
    const id = ids[index];
    body[offset++] = id.length;
    body.set(id, offset);
    offset += id.length;

    view.setUint16(offset, mutation.hints.length, false);
    offset += 2;
    for (const hint of mutation.hints) {
      view.setUint32(offset, hint.beforeFrom, false);
      view.setUint32(offset + 4, hint.beforeTo, false);
      view.setUint32(offset + 8, hint.afterFrom, false);
      view.setUint32(offset + 12, hint.afterTo, false);
      offset += 16;
    }

    view.setUint32(offset, mutation.update.length, false);
    offset += 4;
    body.set(mutation.update, offset);
    offset += mutation.update.length;
  });

  return encode(Tag.AnchoredBatch, docId, body);
}

/**
 * Decode an AnchoredBatch without trusting any length or count in the frame.
 * `null` covers every unsupported, truncated, oversized, or trailing form and
 * keeps the WebSocket message handler total over arbitrary bytes.
 */
export function decodeAnchoredBatch(frame: Frame): AnchoredBatch | null {
  if (frame.tag !== Tag.AnchoredBatch) return null;
  const body = frame.body;
  if (body.length < 3 || body[0] !== ANCHORED_BATCH_VERSION) return null;

  const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
  let offset = 1;
  const mutationCount = view.getUint16(offset, false);
  offset += 2;
  if (mutationCount < 1 || mutationCount > MAX_ANCHORED_MUTATIONS) return null;

  const mutations: AnchoredMutation[] = [];
  const decoder = new TextDecoder("utf-8", { fatal: true });
  for (let mutationIndex = 0; mutationIndex < mutationCount; mutationIndex++) {
    if (body.length - offset < 2) return null;
    const source = SOURCE_BY_CODE.get(body[offset++]);
    if (!source) return null;

    const idLength = body[offset++];
    if (
      idLength < 1 ||
      idLength > MAX_CLIENT_EVENT_ID_BYTES ||
      body.length - offset < idLength
    ) {
      return null;
    }
    let clientEventId: string;
    try {
      clientEventId = decoder.decode(body.subarray(offset, offset + idLength));
    } catch {
      return null;
    }
    offset += idLength;

    if (body.length - offset < 2) return null;
    const hintCount = view.getUint16(offset, false);
    offset += 2;
    if (hintCount > MAX_ANCHORED_HINTS || body.length - offset < hintCount * 16) {
      return null;
    }
    const hints: AnchoredRangeHint[] = [];
    for (let hintIndex = 0; hintIndex < hintCount; hintIndex++) {
      const beforeFrom = view.getUint32(offset, false);
      const beforeTo = view.getUint32(offset + 4, false);
      const afterFrom = view.getUint32(offset + 8, false);
      const afterTo = view.getUint32(offset + 12, false);
      offset += 16;
      if (beforeFrom > beforeTo || afterFrom > afterTo) return null;
      hints.push({ beforeFrom, beforeTo, afterFrom, afterTo });
    }

    if (body.length - offset < 4) return null;
    const updateLength = view.getUint32(offset, false);
    offset += 4;
    if (updateLength < 1 || updateLength > body.length - offset) return null;
    const update = body.slice(offset, offset + updateLength);
    offset += updateLength;
    mutations.push({ source, clientEventId, hints, update });
  }

  if (offset !== body.length) return null;
  return { version: ANCHORED_BATCH_VERSION, mutations };
}
