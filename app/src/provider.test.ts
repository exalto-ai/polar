/**
 * The provider is where AD-16 and AD-17 live, and every bug in it so far was
 * found by driving a browser by hand. These are the cases that cost the most
 * to find that way.
 */
import { Awareness } from "y-protocols/awareness";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as Y from "yjs";
import {
  decode,
  decodeAnchoredBatch,
  encode,
  LocalInputSource,
  Tag,
  type AnchoredMutation,
  type AnchoredRangeHint,
} from "./protocol";
import {
  MAX_PENDING_OUTBOUND_BYTES,
  MAX_PENDING_OUTBOUND_RUNS,
  SyncProvider,
} from "./provider";

/** A socket we drive by hand, so nothing depends on timing or a real server. */
class FakeSocket {
  static instances: FakeSocket[] = [];
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;

  readyState = FakeSocket.CONNECTING;
  binaryType = "";
  sent: Uint8Array[] = [];
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: ArrayBuffer }) => void) | null = null;
  onclose: (() => void) | null = null;

  constructor(
    readonly url: string,
    readonly protocols?: string[],
  ) {
    FakeSocket.instances.push(this);
  }

  send(data: Uint8Array) {
    this.sent.push(data);
  }

  open() {
    this.readyState = FakeSocket.OPEN;
    this.onopen?.();
  }

  close() {
    this.readyState = 3;
    this.onclose?.();
  }

  deliver(bytes: Uint8Array) {
    const copy = bytes.slice();
    this.onmessage?.({ data: copy.buffer as ArrayBuffer });
  }
}

/** A document with some content, and the update frame that produces it. */
function updateFor(text: string): Uint8Array {
  const doc = new Y.Doc();
  doc.getXmlFragment("content");
  const before = Y.encodeStateVector(doc);
  const fragment = doc.getXmlFragment("content");
  const element = new Y.XmlElement("paragraph");
  fragment.push([element]);
  element.push([new Y.XmlText(text)]);
  return Y.encodeStateAsUpdate(doc, before);
}

function emptyUpdate(): Uint8Array {
  return Y.encodeStateAsUpdate(new Y.Doc());
}

function appendParagraph(target: Y.Doc, text: string) {
  const element = new Y.XmlElement("paragraph");
  element.push([new Y.XmlText(text)]);
  target.getXmlFragment("content").push([element]);
}

function sentBatches(target: FakeSocket): Uint8Array[] {
  return target.sent.filter((frame) => frame[0] === Tag.AnchoredBatch);
}

function batchMutations(frame: Uint8Array): AnchoredMutation[] {
  const decoded = decode(frame);
  const batch = decoded && decodeAnchoredBatch(decoded);
  if (!batch) throw new Error("expected an AnchoredBatch frame");
  return batch.mutations;
}

function sentMutations(target: FakeSocket): AnchoredMutation[] {
  return sentBatches(target).flatMap(batchMutations);
}

function replicaFrom(frames: Uint8Array[]): Y.Doc {
  const replica = new Y.Doc();
  for (const frame of frames) {
    for (const mutation of batchMutations(frame)) {
      Y.applyUpdate(replica, mutation.update);
    }
  }
  return replica;
}

let doc: Y.Doc;
let awareness: Awareness;
let provider: SyncProvider;
let socket: FakeSocket;
let transactions: number;

beforeEach(() => {
  FakeSocket.instances = [];
  vi.stubGlobal("WebSocket", FakeSocket);

  doc = new Y.Doc();
  awareness = new Awareness(doc);
  transactions = 0;
  doc.on("update", () => {
    transactions += 1;
  });

  provider = new SyncProvider("ws://test/sync", "tok", "doc-1", doc, awareness);
  provider.connect();
  socket = FakeSocket.instances[0];
  socket.open();
  socket.deliver(encode(Tag.Sync, "doc-1", emptyUpdate()));
});

afterEach(() => {
  provider.destroy();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const settle = () => new Promise((r) => setTimeout(r, 120));

describe("connecting", () => {
  it("reports connecting before the first socket opens", () => {
    const otherDoc = new Y.Doc();
    const other = new SyncProvider(
      "ws://test/sync",
      "tok",
      "doc-connecting",
      otherDoc,
      new Awareness(otherDoc),
    );
    const seen: string[] = [];

    other.subscribeSaveStatus((status) => seen.push(status));
    expect(seen).toEqual(["connecting"]);

    other.connect();
    const otherSocket = FakeSocket.instances[FakeSocket.instances.length - 1];
    otherSocket.open();
    otherSocket.deliver(encode(Tag.Sync, "doc-connecting", emptyUpdate()));
    expect(seen).toEqual(["connecting", "saved"]);
    other.destroy();
  });

  it("carries the editor capability as a subprotocol, never in the URL", () => {
    // The browser WebSocket API cannot set headers; an editor capability in the
    // URL would reach logs and history.
    expect(socket.url).not.toContain("tok");
    expect(socket.protocols).toContain("thought.token.tok");
    expect(socket.protocols).toContain("thought.v1");
  });

  it("announces what it already has, so the daemon sends only the difference", () => {
    expect(socket.sent).toHaveLength(1);
    expect(socket.sent[0][0]).toBe(Tag.Subscribe);
  });
});

describe("initial sync barrier", () => {
  it("keeps a blank document queued and unsent until its first Sync is applied", () => {
    const blank = new Y.Doc();
    const blankAwareness = new Awareness(blank);
    const gated = new SyncProvider(
      "ws://test/sync",
      "tok",
      "doc-blank",
      blank,
      blankAwareness,
    );
    gated.connect();
    const gatedSocket = FakeSocket.instances[FakeSocket.instances.length - 1];
    gatedSocket.open();

    appendParagraph(blank, "queued before sync");
    expect(gated.isHydrated).toBe(false);
    expect(sentBatches(gatedSocket)).toHaveLength(0);
    expect(gatedSocket.sent.map((frame) => frame[0])).toEqual([Tag.Subscribe]);

    gatedSocket.deliver(encode(Tag.Sync, "doc-blank", updateFor("")));
    expect(gated.isHydrated).toBe(true);
    // The daemon-created empty paragraph is present before the queued direct
    // Yjs change is allowed onto the wire. Real editor input cannot create the
    // second root because createEditor remains read-only at this point.
    expect(blank.getXmlFragment("content").length).toBe(2);
    expect(sentBatches(gatedSocket)).toHaveLength(1);
    expect(batchMutations(sentBatches(gatedSocket)[0])).toHaveLength(1);

    gated.destroy();
    blankAwareness.destroy();
    blank.destroy();
  });

  it("applies imported content before releasing the editor barrier", () => {
    const imported = new Y.Doc();
    const importedAwareness = new Awareness(imported);
    const gated = new SyncProvider(
      "ws://test/sync",
      "tok",
      "doc-imported",
      imported,
      importedAwareness,
    );
    const observations: string[] = [];
    gated.subscribeHydration((hydrated) => {
      observations.push(
        `${hydrated}:${imported.getXmlFragment("content").toString()}`,
      );
    });
    gated.connect();
    const gatedSocket = FakeSocket.instances[FakeSocket.instances.length - 1];
    gatedSocket.open();

    expect(observations).toEqual(["false:"]);
    gatedSocket.deliver(
      encode(Tag.Sync, "doc-imported", updateFor("Imported body")),
    );

    expect(gated.isHydrated).toBe(true);
    expect(observations).toEqual([
      "false:",
      "true:<paragraph>Imported body</paragraph>",
    ]);
    expect(sentBatches(gatedSocket)).toHaveLength(0);

    gated.destroy();
    importedAwareness.destroy();
    imported.destroy();
  });
});

describe("coalescing (AD-16)", () => {
  it("applies a burst as one transaction, not one per update", async () => {
    const before = transactions;
    for (let i = 0; i < 25; i++) {
      socket.deliver(encode(Tag.Broadcast, "doc-1", updateFor(`block ${i}`)));
    }
    // Buffered, not yet applied — that is the whole point.
    expect(transactions).toBe(before);
    expect(provider.pending).toBe(25);

    await settle();
    expect(transactions).toBe(before + 1);
    expect(provider.pending).toBe(0);
  });

  it("still flushes when the window is hidden and no frames are painted", async () => {
    // requestAnimationFrame does not fire in a hidden window. A frame-only
    // schedule stalls forever there: the document silently stops updating and
    // then lurches when refocused.
    const raf = vi.spyOn(globalThis, "requestAnimationFrame").mockImplementation(() => 1);

    socket.deliver(encode(Tag.Broadcast, "doc-1", updateFor("while hidden")));
    expect(provider.pending).toBe(1);

    await settle();
    expect(provider.pending).toBe(0);
    expect(doc.getXmlFragment("content").length).toBeGreaterThan(0);
    raf.mockRestore();
  });
});

describe("the composition guard (AD-17)", () => {
  it("holds remote updates while an input method is composing", async () => {
    provider.setComposing(true);
    socket.deliver(encode(Tag.Broadcast, "doc-1", updateFor("arrives mid-composition")));

    await settle();
    // y-prosemirror has no guard of its own: applying here would redraw the
    // node the input method has live marked text in.
    expect(provider.pending).toBe(1);
    expect(doc.getXmlFragment("content").length).toBe(0);

    provider.setComposing(false);
    await settle();
    expect(provider.pending).toBe(0);
    expect(doc.getXmlFragment("content").length).toBeGreaterThan(0);
  });

  it("delivers everything buffered during a long composition at once", async () => {
    provider.setComposing(true);
    for (let i = 0; i < 5; i++) {
      socket.deliver(encode(Tag.Broadcast, "doc-1", updateFor(`held ${i}`)));
    }
    await settle();
    const before = transactions;

    provider.setComposing(false);
    await settle();
    expect(transactions).toBe(before + 1);
  });
});

describe("echo and origin", () => {
  it("does not send back what it received", async () => {
    socket.sent.length = 0;
    socket.deliver(encode(Tag.Broadcast, "doc-1", updateFor("from a peer")));
    await settle();
    // Applied with the REMOTE origin, so the local-update handler ignores it.
    // Without that, two peers would trade the same update forever.
    expect(socket.sent).toHaveLength(0);
  });

  it("publishes local edits", async () => {
    socket.sent.length = 0;
    const element = new Y.XmlElement("paragraph");
    doc.getXmlFragment("content").push([element]);
    await settle();
    expect(socket.sent.some((f) => f[0] === Tag.AnchoredBatch)).toBe(true);
    expect(sentMutations(socket)).toMatchObject([
      { source: LocalInputSource.Unknown, hints: [] },
    ]);
  });

  it("ignores frames for other documents", async () => {
    const before = transactions;
    socket.deliver(encode(Tag.Broadcast, "someone-elses-doc", updateFor("nope")));
    await settle();
    expect(transactions).toBe(before);
  });
});

describe("anchored outbound batching", () => {
  it("merges Yjs emissions inside one dispatch but preserves transaction boundaries", () => {
    socket.sent.length = 0;
    const pasteHints: AnchoredRangeHint[] = [
      { beforeFrom: 2, beforeTo: 2, afterFrom: 2, afterTo: 12 },
    ];

    provider.withLocalTransaction(LocalInputSource.Written, [], () => {
      appendParagraph(doc, "written head");
    });
    provider.withLocalTransaction(LocalInputSource.Paste, pasteHints, () => {
      appendParagraph(doc, "pasted one");
      appendParagraph(doc, "pasted two");
    });
    provider.withLocalTransaction(LocalInputSource.Written, [], () => {
      appendParagraph(doc, "written tail");
    });

    // Two internal Yjs emissions from the paste dispatch are one mutation.
    // The later written transaction remains distinct even though its source
    // matches the in-flight head.
    expect(provider.pendingOutbound).toBe(3);
    expect(sentBatches(socket)).toHaveLength(1);
    expect(sentMutations(socket)[0].source).toBe(LocalInputSource.Written);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentBatches(socket)).toHaveLength(2);
    const tail = batchMutations(sentBatches(socket)[1]);
    expect(tail.map((item) => item.source)).toEqual([
      LocalInputSource.Paste,
      LocalInputSource.Written,
    ]);
    expect(tail[0].hints).toEqual(pasteHints);
    expect(new Set(sentMutations(socket).map((item) => item.clientEventId)).size).toBe(3);

    const replica = replicaFrom(sentBatches(socket));
    const content = replica.getXmlFragment("content").toString();
    expect(content).toContain("written head");
    expect(content).toContain("pasted one");
    expect(content).toContain("pasted two");
    expect(content).toContain("written tail");
  });

  it("retries the identical in-flight batch before later edits after a lost ACK", async () => {
    vi.useFakeTimers();
    socket.sent.length = 0;

    provider.withLocalTransaction(LocalInputSource.Written, [], () => {
      appendParagraph(doc, "head");
    });
    const original = sentBatches(socket)[0].slice();
    const originalMutation = batchMutations(original)[0];

    provider.withLocalTransaction(LocalInputSource.Written, [], () => {
      appendParagraph(doc, "same-source tail");
    });
    provider.withLocalTransaction(LocalInputSource.Paste, [], () => {
      appendParagraph(doc, "different-source tail");
    });
    expect(provider.pendingOutbound).toBe(3);

    socket.close();
    expect(provider.pendingOutbound).toBe(3);

    await vi.advanceTimersByTimeAsync(250);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    expect(sentBatches(reconnected)).toHaveLength(1);
    expect(sentBatches(reconnected)[0]).toEqual(original);
    expect(batchMutations(sentBatches(reconnected)[0])[0].clientEventId).toBe(
      originalMutation.clientEventId,
    );

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentBatches(reconnected)).toHaveLength(2);
    expect(batchMutations(sentBatches(reconnected)[1]).map((item) => item.source)).toEqual([
      LocalInputSource.Written,
      LocalInputSource.Paste,
    ]);
  });

  it("keeps the sent head immutable while batching the next 128 mutations", () => {
    socket.sent.length = 0;

    appendParagraph(doc, "head");
    const head = sentBatches(socket)[0].slice();
    expect(batchMutations(head)).toHaveLength(1);
    expect(provider.pendingOutbound).toBe(1);

    for (let index = 0; index < 128; index++) {
      appendParagraph(doc, `tail ${index}`);
    }

    expect(sentBatches(socket)).toEqual([head]);
    expect(provider.pendingOutbound).toBe(129);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    const frames = sentBatches(socket);
    expect(frames).toHaveLength(2);
    expect(batchMutations(frames[1])).toHaveLength(128);
    expect(provider.pendingOutbound).toBe(128);

    const replica = replicaFrom(frames);
    expect(replica.getXmlFragment("content").length).toBe(129);
    expect(replica.getXmlFragment("content").toString()).toContain("tail 127");

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(provider.pendingOutbound).toBe(0);
    expect(provider.hasPendingChanges).toBe(false);
  });

  it("drains the largest retained offline session in ordered batches", async () => {
    vi.useFakeTimers();
    const seen: string[] = [];
    provider.subscribeSaveStatus((status) => seen.push(status));
    socket.sent.length = 0;
    socket.close();
    for (let index = 0; index < MAX_PENDING_OUTBOUND_RUNS; index++) {
      appendParagraph(doc, `offline ${index}`);
    }
    expect(provider.pendingOutbound).toBe(MAX_PENDING_OUTBOUND_RUNS);
    expect(seen[seen.length - 1]).toBe("offline");

    await vi.advanceTimersByTimeAsync(250);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    expect(batchMutations(sentBatches(reconnected)[0])).toHaveLength(128);
    expect(provider.pendingOutbound).toBe(MAX_PENDING_OUTBOUND_RUNS);
    expect(seen[seen.length - 1]).toBe("saving");

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(batchMutations(sentBatches(reconnected)[1])).toHaveLength(128);
    expect(provider.pendingOutbound).toBe(128);

    const frames = sentBatches(reconnected);
    const ids = frames.flatMap(batchMutations).map((item) => item.clientEventId);
    expect(new Set(ids).size).toBe(MAX_PENDING_OUTBOUND_RUNS);
    const replica = replicaFrom(frames);
    expect(replica.getXmlFragment("content").length).toBe(MAX_PENDING_OUTBOUND_RUNS);
    expect(replica.getXmlFragment("content").toString()).toContain("offline 255");

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(provider.pendingOutbound).toBe(0);
    expect(provider.hasPendingChanges).toBe(false);
    expect(seen[seen.length - 1]).toBe("saved");
  });

  it("bounds ten thousand alternating source runs and degrades overflow to Unknown", async () => {
    vi.useFakeTimers();
    socket.sent.length = 0;
    socket.close();

    for (let index = 0; index < 10_000; index++) {
      const source = index % 2 === 0 ? LocalInputSource.Written : LocalInputSource.Paste;
      provider.withLocalInputSource(source, () => appendParagraph(doc, `alternating ${index}`));
      expect(provider.pendingOutbound).toBeLessThanOrEqual(MAX_PENDING_OUTBOUND_RUNS);
      expect(provider.pendingOutboundBytes).toBeLessThanOrEqual(MAX_PENDING_OUTBOUND_BYTES);
    }

    expect(provider.pendingOutbound).toBe(1);
    expect(provider.pendingOutboundBytes).toBe(0);

    await vi.advanceTimersByTimeAsync(250);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    const frames = sentBatches(reconnected);
    expect(frames).toHaveLength(1);
    expect(batchMutations(frames[0])).toMatchObject([
      { source: LocalInputSource.Unknown, hints: [] },
    ]);
    expect(replicaFrom(frames).getXmlFragment("content").length).toBe(10_000);

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(provider.pendingOutbound).toBe(0);
  });

  it("does not retain an oversized local update in the bounded outbox", async () => {
    vi.useFakeTimers();
    socket.sent.length = 0;
    socket.close();

    const text = "x".repeat(MAX_PENDING_OUTBOUND_BYTES * 2);
    provider.withLocalInputSource(LocalInputSource.Paste, () => appendParagraph(doc, text));
    expect(provider.pendingOutbound).toBe(1);
    expect(provider.pendingOutboundBytes).toBe(0);

    await vi.advanceTimersByTimeAsync(250);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    const frames = sentBatches(reconnected);
    expect(frames).toHaveLength(1);
    expect(batchMutations(frames[0])).toMatchObject([
      { source: LocalInputSource.Unknown, hints: [] },
    ]);
    expect(replicaFrom(frames).getXmlFragment("content").toString()).toContain(text);
  });

  it("stays bounded when an oversized snapshot retry includes more local edits", async () => {
    vi.useFakeTimers();
    socket.sent.length = 0;
    socket.close();

    const head = "x".repeat(MAX_PENDING_OUTBOUND_BYTES * 2);
    provider.withLocalInputSource(LocalInputSource.Paste, () => appendParagraph(doc, head));
    expect(provider.pendingOutbound).toBe(1);
    expect(provider.pendingOutboundBytes).toBe(0);

    await vi.advanceTimersByTimeAsync(250);
    const firstRetry = FakeSocket.instances[FakeSocket.instances.length - 1];
    firstRetry.open();
    const firstMutation = batchMutations(sentBatches(firstRetry)[0])[0];
    expect(firstMutation.source).toBe(LocalInputSource.Unknown);

    for (let index = 0; index < 256; index++) {
      provider.withLocalInputSource(LocalInputSource.Paste, () =>
        appendParagraph(doc, `during retry ${index}`),
      );
      expect(provider.pendingOutbound).toBeLessThanOrEqual(MAX_PENDING_OUTBOUND_RUNS);
      expect(provider.pendingOutboundBytes).toBeLessThanOrEqual(MAX_PENDING_OUTBOUND_BYTES);
    }
    firstRetry.close();
    expect(provider.pendingOutbound).toBe(1);
    expect(provider.pendingOutboundBytes).toBe(0);

    await vi.advanceTimersByTimeAsync(500);
    const secondRetry = FakeSocket.instances[FakeSocket.instances.length - 1];
    secondRetry.open();
    const frames = sentBatches(secondRetry);
    expect(frames).toHaveLength(1);
    const secondMutation = batchMutations(frames[0])[0];
    expect(secondMutation.source).toBe(LocalInputSource.Unknown);
    expect(secondMutation.clientEventId).not.toBe(firstMutation.clientEventId);
    const replica = replicaFrom(frames);
    expect(replica.getXmlFragment("content").length).toBe(257);
    expect(replica.getXmlFragment("content").toString()).toContain(head);
    expect(replica.getXmlFragment("content").toString()).toContain("during retry 255");
  });
});

describe("durable save status", () => {
  it("stays saving until the daemon acknowledges every local update", () => {
    const seen: string[] = [];
    provider.subscribeSaveStatus((status) => seen.push(status));
    socket.sent.length = 0;

    const first = new Y.XmlElement("paragraph");
    doc.getXmlFragment("content").push([first]);
    const second = new Y.XmlElement("paragraph");
    doc.getXmlFragment("content").push([second]);

    // Keep exactly one update in flight. This makes an Error or ACK refer to a
    // known queue entry instead of relying on every response being successful.
    expect(sentBatches(socket)).toHaveLength(1);
    expect(provider.hasPendingChanges).toBe(true);
    expect(seen).toEqual(["saved", "saving"]);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("saving");
    expect(sentBatches(socket)).toHaveLength(2);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("saved");
    expect(provider.hasPendingChanges).toBe(false);
  });

  it("does not let an unrelated or unsolicited ack mark work as saved", () => {
    const seen: string[] = [];
    provider.subscribeSaveStatus((status) => seen.push(status));
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);

    socket.deliver(encode(Tag.Ack, "someone-elses-doc", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("saving");

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("saved");
    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("saved");
  });

  it("keeps sent and offline edits queued across a reconnect", async () => {
    vi.useFakeTimers();
    const seen: string[] = [];
    provider.subscribeSaveStatus((status) => seen.push(status));
    socket.sent.length = 0;

    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    expect(sentBatches(socket)).toHaveLength(1);

    socket.close();
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    expect(seen[seen.length - 1]).toBe("offline");

    await vi.advanceTimersByTimeAsync(250);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    expect(reconnected.sent[0][0]).toBe(Tag.Subscribe);
    expect(sentBatches(reconnected)).toHaveLength(1);
    expect(seen[seen.length - 1]).toBe("saving");

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentBatches(reconnected)).toHaveLength(2);
    expect(seen[seen.length - 1]).toBe("saving");

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("saved");
  });

  it("does not let an ACK after Error consume the failed FIFO entry", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.useFakeTimers();
    const seen: string[] = [];
    provider.subscribeSaveStatus((status) => seen.push(status));
    socket.sent.length = 0;

    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    expect(sentBatches(socket)).toHaveLength(1);

    socket.deliver(encode(Tag.Error, "doc-1", new TextEncoder().encode("disk full")));
    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("error");
    expect(provider.hasPendingChanges).toBe(true);
    expect(sentBatches(socket)).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(1_000);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    expect(sentBatches(reconnected)).toHaveLength(1);

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentBatches(reconnected)).toHaveLength(2);
    expect(provider.hasPendingChanges).toBe(true);

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(provider.hasPendingChanges).toBe(false);
    expect(seen[seen.length - 1]).toBe("saved");
  });

  it("reports a persistence error without discarding the queued update", async () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.useFakeTimers();
    const seen: string[] = [];
    provider.subscribeSaveStatus((status) => seen.push(status));
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);

    socket.deliver(encode(Tag.Error, "doc-1", new TextEncoder().encode("disk full")));
    expect(seen[seen.length - 1]).toBe("error");

    await vi.advanceTimersByTimeAsync(1_000);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    expect(sentBatches(reconnected)).toHaveLength(1);
  });

  it("keeps a persistence error visible when more edits arrive during backoff", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    vi.useFakeTimers();
    const seen: string[] = [];
    provider.subscribeSaveStatus((status) => seen.push(status));
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);

    socket.deliver(encode(Tag.Error, "doc-1", new TextEncoder().encode("disk full")));
    expect(seen[seen.length - 1]).toBe("error");

    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    expect(seen[seen.length - 1]).toBe("error");
    expect(provider.hasPendingChanges).toBe(true);
  });

  it("waits for durable save and times out without dropping pending work", async () => {
    vi.useFakeTimers();
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    const saved = provider.waitUntilSaved(500);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    await expect(saved).resolves.toBe(true);

    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    const timedOut = provider.waitUntilSaved(500);
    await vi.advanceTimersByTimeAsync(500);
    await expect(timedOut).resolves.toBe(false);
    expect(provider.hasPendingChanges).toBe(true);
  });

  it("returns immediately when there is no local work to save", async () => {
    expect(provider.hasPendingChanges).toBe(false);
    await expect(provider.waitUntilSaved(1)).resolves.toBe(true);
  });

  it("releases a save waiter when the provider is destroyed", async () => {
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    const saved = provider.waitUntilSaved(10_000);

    provider.destroy();
    await expect(saved).resolves.toBe(false);
  });

  it("delivers the current state immediately and can unsubscribe", () => {
    const seen: string[] = [];
    const unsubscribe = provider.subscribeSaveStatus((status) => seen.push(status));
    expect(seen).toEqual(["saved"]);

    unsubscribe();
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    expect(seen).toEqual(["saved"]);
  });
});

describe("losing the daemon", () => {
  it("reports offline and retries", async () => {
    const seen: string[] = [];
    const p = new SyncProvider("ws://test/sync", "tok", "doc-2", new Y.Doc(), awareness, (s) =>
      seen.push(s),
    );
    p.connect();
    const s = FakeSocket.instances[FakeSocket.instances.length - 1];
    s.open();
    expect(seen).toEqual(["connecting", "connected"]);

    const opened = FakeSocket.instances.length;
    s.close();
    expect(seen).toContain("offline");

    // The daemon restarting is a normal event, not an outage.
    await new Promise((r) => setTimeout(r, 400));
    expect(FakeSocket.instances.length).toBeGreaterThan(opened);
    p.destroy();
  });

  it("does not reconnect a provider destroyed during backoff", async () => {
    vi.useFakeTimers();
    const opened = FakeSocket.instances.length;

    socket.close();
    provider.destroy();
    await vi.advanceTimersByTimeAsync(5_000);

    expect(FakeSocket.instances).toHaveLength(opened);
  });
});
