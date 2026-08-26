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
  decodeSourcedUpdate,
  encode,
  LocalInputSource,
  Tag,
} from "./protocol";
import { SyncProvider } from "./provider";

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

function appendParagraph(target: Y.Doc, text: string) {
  const element = new Y.XmlElement("paragraph");
  element.push([new Y.XmlText(text)]);
  target.getXmlFragment("content").push([element]);
}

function sentUpdates(target: FakeSocket): Uint8Array[] {
  return target.sent.filter((frame) => frame[0] === Tag.SourcedUpdate);
}

function updateBody(frame: Uint8Array): Uint8Array {
  const decoded = decode(frame);
  const sourced = decoded && decodeSourcedUpdate(decoded);
  if (!sourced) throw new Error("expected a SourcedUpdate frame");
  return sourced.update;
}

function updateSource(frame: Uint8Array) {
  const decoded = decode(frame);
  const sourced = decoded && decodeSourcedUpdate(decoded);
  if (!sourced) throw new Error("expected a SourcedUpdate frame");
  return sourced.source;
}

function replicaFrom(frames: Uint8Array[]): Y.Doc {
  const replica = new Y.Doc();
  for (const frame of frames) Y.applyUpdate(replica, updateBody(frame));
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
    FakeSocket.instances[FakeSocket.instances.length - 1].open();
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
    expect(socket.sent.some((f) => f[0] === Tag.SourcedUpdate)).toBe(true);
    expect(updateSource(sentUpdates(socket)[0])).toBe(LocalInputSource.Unknown);
  });

  it("ignores frames for other documents", async () => {
    const before = transactions;
    socket.deliver(encode(Tag.Broadcast, "someone-elses-doc", updateFor("nope")));
    await settle();
    expect(transactions).toBe(before);
  });
});

describe("outbound coalescing", () => {
  it("coalesces adjacent equal sources without crossing source boundaries", () => {
    socket.sent.length = 0;

    provider.withLocalInputSource(LocalInputSource.Written, () => {
      appendParagraph(doc, "written head");
    });
    provider.withLocalInputSource(LocalInputSource.Paste, () => {
      appendParagraph(doc, "pasted one");
      appendParagraph(doc, "pasted two");
    });
    provider.withLocalInputSource(LocalInputSource.Written, () => {
      appendParagraph(doc, "written tail");
    });

    // The first source run is immutable in flight. The two adjacent paste
    // updates merge, while the later written run remains a separate entry.
    expect(provider.pendingOutbound).toBe(3);
    expect(sentUpdates(socket)).toHaveLength(1);
    expect(updateSource(sentUpdates(socket)[0])).toBe(LocalInputSource.Written);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentUpdates(socket)).toHaveLength(2);
    expect(updateSource(sentUpdates(socket)[1])).toBe(LocalInputSource.Paste);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentUpdates(socket)).toHaveLength(3);
    expect(updateSource(sentUpdates(socket)[2])).toBe(LocalInputSource.Written);

    const replica = replicaFrom(sentUpdates(socket));
    const content = replica.getXmlFragment("content").toString();
    expect(content).toContain("written head");
    expect(content).toContain("pasted one");
    expect(content).toContain("pasted two");
    expect(content).toContain("written tail");
  });

  it("merges an interrupted head only with an adjacent run of the same source", async () => {
    vi.useFakeTimers();
    socket.sent.length = 0;

    provider.withLocalInputSource(LocalInputSource.Written, () => {
      appendParagraph(doc, "head");
      appendParagraph(doc, "same-source tail");
    });
    provider.withLocalInputSource(LocalInputSource.Paste, () => {
      appendParagraph(doc, "different-source tail");
    });
    expect(provider.pendingOutbound).toBe(3);

    socket.close();
    // The unacknowledged written head and adjacent written tail are both
    // unsent now, so they become one run. Paste remains separate.
    expect(provider.pendingOutbound).toBe(2);

    await vi.advanceTimersByTimeAsync(250);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    expect(sentUpdates(reconnected)).toHaveLength(1);
    expect(updateSource(sentUpdates(reconnected)[0])).toBe(LocalInputSource.Written);

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentUpdates(reconnected)).toHaveLength(2);
    expect(updateSource(sentUpdates(reconnected)[1])).toBe(LocalInputSource.Paste);
  });

  it("preserves the in-flight head and merges every later edit into one tail", () => {
    const emitted: Uint8Array[] = [];
    doc.on("update", (update) => emitted.push(update.slice()));
    socket.sent.length = 0;

    appendParagraph(doc, "head");
    expect(sentUpdates(socket)).toHaveLength(1);
    expect(updateBody(sentUpdates(socket)[0])).toEqual(emitted[0]);
    expect(provider.pendingOutbound).toBe(1);

    for (let index = 0; index < 128; index++) {
      appendParagraph(doc, `tail ${index}`);
    }

    // The first frame is already on the wire and its positional ACK can only
    // consume that exact head. The other 128 updates occupy one merged tail.
    expect(sentUpdates(socket)).toHaveLength(1);
    expect(updateBody(sentUpdates(socket)[0])).toEqual(emitted[0]);
    expect(provider.pendingOutbound).toBe(2);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    const frames = sentUpdates(socket);
    expect(frames).toHaveLength(2);
    expect(updateBody(frames[1])).toEqual(Y.mergeUpdates(emitted.slice(1)));
    expect(provider.pendingOutbound).toBe(1);

    const replica = replicaFrom(frames);
    expect(replica.getXmlFragment("content").length).toBe(129);
    expect(replica.getXmlFragment("content").toString()).toContain("tail 127");

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(provider.pendingOutbound).toBe(0);
    expect(provider.hasPendingChanges).toBe(false);
  });

  it("bounds a long offline session and drains it in one reconnect round-trip", async () => {
    vi.useFakeTimers();
    const seen: string[] = [];
    provider.subscribeSaveStatus((status) => seen.push(status));
    socket.sent.length = 0;

    appendParagraph(doc, "sent before disconnect");
    appendParagraph(doc, "queued before disconnect");
    expect(provider.pendingOutbound).toBe(2);

    socket.close();
    expect(provider.pendingOutbound).toBe(1);
    for (let index = 0; index < 256; index++) {
      appendParagraph(doc, `offline ${index}`);
      expect(provider.pendingOutbound).toBe(1);
    }
    expect(seen[seen.length - 1]).toBe("offline");

    await vi.advanceTimersByTimeAsync(250);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    const frames = sentUpdates(reconnected);
    expect(frames).toHaveLength(1);
    expect(provider.pendingOutbound).toBe(1);
    expect(seen[seen.length - 1]).toBe("saving");

    const replica = replicaFrom(frames);
    expect(replica.getXmlFragment("content").length).toBe(258);
    const content = replica.getXmlFragment("content").toString();
    expect(content).toContain("sent before disconnect");
    expect(content).toContain("queued before disconnect");
    expect(content).toContain("offline 255");

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentUpdates(reconnected)).toHaveLength(1);
    expect(provider.pendingOutbound).toBe(0);
    expect(provider.hasPendingChanges).toBe(false);
    expect(seen[seen.length - 1]).toBe("saved");
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
    expect(sentUpdates(socket)).toHaveLength(1);
    expect(provider.hasPendingChanges).toBe(true);
    expect(seen).toEqual(["saved", "saving"]);

    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("saving");
    expect(sentUpdates(socket)).toHaveLength(2);

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
    expect(sentUpdates(socket)).toHaveLength(1);

    socket.close();
    doc.getXmlFragment("content").push([new Y.XmlElement("paragraph")]);
    expect(seen[seen.length - 1]).toBe("offline");

    await vi.advanceTimersByTimeAsync(250);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    expect(reconnected.sent[0][0]).toBe(Tag.Subscribe);
    expect(sentUpdates(reconnected)).toHaveLength(1);
    expect(seen[seen.length - 1]).toBe("saving");

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentUpdates(reconnected)).toHaveLength(1);
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
    expect(sentUpdates(socket)).toHaveLength(1);

    socket.deliver(encode(Tag.Error, "doc-1", new TextEncoder().encode("disk full")));
    socket.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(seen[seen.length - 1]).toBe("error");
    expect(provider.hasPendingChanges).toBe(true);
    expect(sentUpdates(socket)).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(1_000);
    const reconnected = FakeSocket.instances[FakeSocket.instances.length - 1];
    reconnected.open();
    expect(sentUpdates(reconnected)).toHaveLength(1);

    reconnected.deliver(encode(Tag.Ack, "doc-1", new Uint8Array()));
    expect(sentUpdates(reconnected)).toHaveLength(1);
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
    expect(sentUpdates(reconnected)).toHaveLength(1);
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
