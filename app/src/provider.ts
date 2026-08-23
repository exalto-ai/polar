/**
 * Binds a Y.Doc to the daemon's sync endpoint, and is where AD-16 and AD-17
 * stop being notes.
 *
 * Both are the same buffer. Coalescing flushes it once per frame; the
 * composition guard adds one more condition to that flush. They were found
 * separately — one by the WKWebView probe, one by reading y-prosemirror — but
 * they are one mechanism.
 */
import * as Y from "yjs";
import { Awareness, applyAwarenessUpdate, encodeAwarenessUpdate } from "y-protocols/awareness";
import { decode, encode, Tag } from "./protocol";

/** Marks transactions that came off the wire, so they are not echoed back. */
export const REMOTE = Symbol("remote");

/** Backstop for when the window is hidden and no frames are painted. */
const BACKGROUND_FLUSH_MS = 50;

export type ProviderStatus = "connecting" | "connected" | "offline";

export type AgentPresence = {
  actor_id: string;
  name: string;
  model: string | null;
  session: string | null;
};

export class SyncProvider {
  private socket: WebSocket | null = null;
  private inbound: Uint8Array[] = [];
  private frame: number | null = null;
  private timer: number | null = null;
  private composing = false;
  private reconnectDelay = 250;
  private closed = false;

  constructor(
    private readonly url: string,
    private readonly token: string,
    private readonly docId: string,
    private readonly doc: Y.Doc,
    private readonly awareness: Awareness,
    private readonly onStatus: (status: ProviderStatus) => void = () => {},
    private readonly onAgent: (agent: AgentPresence) => void = () => {},
  ) {
    this.doc.on("update", this.onLocalUpdate);
    this.awareness.on("update", this.onAwarenessChange);
  }

  connect() {
    this.onStatus("connecting");
    // The browser WebSocket API cannot set headers, so the token rides as a
    // subprotocol. It is header-borne and never part of the URL, which would
    // otherwise put a bearer credential into logs and history.
    const socket = new WebSocket(this.url, ["thought.v1", `thought.token.${this.token}`]);
    socket.binaryType = "arraybuffer";
    this.socket = socket;

    socket.onopen = () => {
      this.reconnectDelay = 250;
      this.onStatus("connected");
      // Announce what we already have; the daemon replies with the difference.
      socket.send(encode(Tag.Subscribe, this.docId, Y.encodeStateVector(this.doc)));
    };

    socket.onmessage = (event) => {
      const frame = decode(new Uint8Array(event.data as ArrayBuffer));
      if (!frame || frame.docId !== this.docId) return;

      switch (frame.tag) {
        case Tag.Sync:
        case Tag.Broadcast:
          this.queue(frame.body);
          break;
        case Tag.Awareness:
          applyAwarenessUpdate(this.awareness, frame.body, REMOTE);
          break;
        case Tag.Presence:
          try {
            this.onAgent(JSON.parse(new TextDecoder().decode(frame.body)));
          } catch {
            // A presence frame we cannot read is not worth dropping the
            // connection over.
          }
          break;
        case Tag.Error:
          console.error("sync error:", new TextDecoder().decode(frame.body));
          break;
      }
    };

    socket.onclose = () => {
      this.onStatus("offline");
      if (this.closed) return;
      // Back off, but stay responsive: the daemon restarting is a normal event,
      // not an outage.
      setTimeout(() => this.connect(), this.reconnectDelay);
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, 5000);
    };
  }

  /**
   * AD-17. Held while an input method has live marked text, because applying a
   * remote update redraws the node being composed in and y-prosemirror has no
   * guard of its own.
   */
  setComposing(composing: boolean) {
    this.composing = composing;
    if (!composing) this.schedule();
  }

  destroy() {
    this.closed = true;
    this.doc.off("update", this.onLocalUpdate);
    this.awareness.off("update", this.onAwarenessChange);
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    if (this.timer !== null) clearTimeout(this.timer);
    this.socket?.close();
  }

  private onLocalUpdate = (update: Uint8Array, origin: unknown) => {
    if (origin === REMOTE) return;
    this.send(encode(Tag.Update, this.docId, update));
  };

  private onAwarenessChange = (
    changes: { added: number[]; updated: number[]; removed: number[] },
    origin: unknown,
  ) => {
    if (origin === REMOTE) return;
    const ids = [...changes.added, ...changes.updated, ...changes.removed];
    this.send(encode(Tag.Awareness, this.docId, encodeAwarenessUpdate(this.awareness, ids)));
  };

  private send(bytes: Uint8Array) {
    if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(bytes);
  }

  private queue(update: Uint8Array) {
    this.inbound.push(update);
    this.schedule();
  }

  /**
   * Flush on the next frame, or on a timer — whichever comes first.
   *
   * `requestAnimationFrame` does not fire at all in a hidden window, so a
   * frame-only schedule stalls the buffer indefinitely whenever the window is
   * behind another window, minimised, or on another Space. The document would
   * silently stop updating and then lurch forward when refocused. `setTimeout`
   * is throttled in the background but still fires, so it is the backstop.
   */
  private schedule() {
    if (this.frame !== null || this.timer !== null) return;
    const run = () => {
      if (this.frame !== null) cancelAnimationFrame(this.frame);
      if (this.timer !== null) clearTimeout(this.timer);
      this.frame = null;
      this.timer = null;
      this.flush();
    };
    this.frame = requestAnimationFrame(run);
    this.timer = window.setTimeout(run, BACKGROUND_FLUSH_MS);
  }

  /**
   * AD-16. One merged transaction per frame rather than one per update.
   *
   * Agents emit dense bursts of block ops, and applying each as its own
   * ProseMirror transaction saturated the main thread badly enough in the probe
   * that updates arrived twenty seconds behind a 120ms link.
   */
  private flush() {
    if (this.composing || this.inbound.length === 0) return;
    const merged = this.inbound.length === 1 ? this.inbound[0] : Y.mergeUpdates(this.inbound);
    this.inbound = [];
    Y.applyUpdate(this.doc, merged, REMOTE);
  }

  /** Pending inbound updates. Exposed so tests can observe the buffer. */
  get pending() {
    return this.inbound.length;
  }
}
