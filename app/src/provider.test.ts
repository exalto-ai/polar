/**
 * The provider is where AD-16 and AD-17 live, and every bug in it so far was
 * found by driving a browser by hand. These are the cases that cost the most
 * to find that way.
 */
import { Awareness } from "y-protocols/awareness";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as Y from "yjs";
import { encode, Tag } from "./protocol";
import { SyncProvider } from "./provider";

/** A socket we drive by hand, so nothing depends on timing or a real server. */
class FakeSocket {
  static instances: FakeSocket[] = [];
  static readonly OPEN = 1;

  readyState = FakeSocket.OPEN;
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
  socket.onopen?.();
});

afterEach(() => {
  provider.destroy();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

const settle = () => new Promise((r) => setTimeout(r, 120));

describe("connecting", () => {
  it("carries the token as a subprotocol, never in the URL", () => {
    // The browser WebSocket API cannot set headers; a token in the URL would
    // reach logs and history.
    expect(socket.url).not.toContain("tok");
    expect(socket.protocols).toContain("polar.token.tok");
    expect(socket.protocols).toContain("polar.v1");
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
    expect(socket.sent.some((f) => f[0] === Tag.Update)).toBe(true);
  });

  it("ignores frames for other documents", async () => {
    const before = transactions;
    socket.deliver(encode(Tag.Broadcast, "someone-elses-doc", updateFor("nope")));
    await settle();
    expect(transactions).toBe(before);
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
    s.onopen?.();
    expect(seen).toEqual(["connecting", "connected"]);

    const opened = FakeSocket.instances.length;
    s.close();
    expect(seen).toContain("offline");

    // The daemon restarting is a normal event, not an outage.
    await new Promise((r) => setTimeout(r, 400));
    expect(FakeSocket.instances.length).toBeGreaterThan(opened);
    p.destroy();
  });
});
