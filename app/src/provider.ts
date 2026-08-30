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
import {
  decode,
  encode,
  encodeEditorMutation,
  Tag,
  type EditorRange,
} from "./protocol";

/** Marks transactions that came off the wire, so they are not echoed back. */
export const REMOTE = Symbol("remote");

/** Backstop for when the window is hidden and no frames are painted. */
const BACKGROUND_FLUSH_MS = 50;

export type ProviderStatus = "connecting" | "connected" | "offline";
export type SaveStatus = "connecting" | "saved" | "saving" | "offline" | "error";

export type AgentPresence = {
  actor_id: string;
  name: string;
  model: string | null;
  session: string | null;
};

/**
 * Acknowledgements are positional, so the update already sent must remain
 * unchanged until its ACK arrives. All later updates can be merged into one
 * tail, which keeps the queue at two entries even during a long editing burst.
 */
type OutboundMutation = { update: Uint8Array; ranges: EditorRange[] };

class OutboundQueue {
  private inFlight: OutboundMutation | null = null;
  private queued: OutboundMutation[] = [];

  get length(): number {
    return Number(this.inFlight !== null) + this.queued.length;
  }

  get hasPending(): boolean {
    return this.inFlight !== null || this.queued.length > 0;
  }

  enqueue(update: Uint8Array, ranges: readonly EditorRange[]) {
    this.queued.push({
      update: update.slice(),
      ranges: ranges.map((range) => ({ ...range })),
    });
  }

  beginSend(): OutboundMutation | null {
    if (this.inFlight !== null || this.queued.length === 0) return null;
    this.inFlight = this.queued.shift()!;
    return this.inFlight;
  }

  /** Consume exactly the head that the peer has durably acknowledged. */
  acknowledge(): boolean {
    if (this.inFlight === null) return false;
    this.inFlight = null;
    return true;
  }

  /**
   * Once the socket is gone, no ACK from it can be accepted. The old head and
   * tail are both unsent work for the next connection and can become one update.
   */
  retryInFlight() {
    if (this.inFlight === null) return;
    this.queued.unshift(this.inFlight);
    this.inFlight = null;
  }
}

export class SyncProvider {
  private socket: WebSocket | null = null;
  private inbound: Uint8Array[] = [];
  private frame: number | null = null;
  private timer: number | null = null;
  private composing = false;
  private reconnectDelay = 250;
  private reconnectTimer: number | null = null;
  private closed = false;
  /** Local updates stay here until the daemon confirms its SQLite commit. */
  private readonly outbound = new OutboundQueue();
  /** An Error freezes the queue until a fresh connection can retry in order. */
  private saveError = false;
  private saveStatus: SaveStatus = "connecting";
  private readonly saveListeners = new Set<(status: SaveStatus) => void>();
  private hydrated = false;
  private initialSyncPending = false;
  private readonly hydrationListeners = new Set<(hydrated: boolean) => void>();
  private activeEditorTransaction: { ranges: EditorRange[]; updates: Uint8Array[] } | null = null;

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
    if (this.closed) return;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (
      this.socket &&
      (this.socket.readyState === WebSocket.CONNECTING ||
        this.socket.readyState === WebSocket.OPEN)
    ) {
      return;
    }

    this.onStatus("connecting");
    this.setSaveStatus("connecting");
    // A new connection retries the exact unacknowledged editor transaction.
    // This is safe when only the ACK was lost because Yjs updates are idempotent.
    this.outbound.retryInFlight();
    this.saveError = false;
    // The browser WebSocket API cannot set headers, so the token rides as a
    // subprotocol. It is header-borne and never part of the URL, which would
    // otherwise put a bearer credential into logs and history.
    const socket = new WebSocket(this.url, ["thought.v1", `thought.token.${this.token}`]);
    socket.binaryType = "arraybuffer";
    this.socket = socket;

    socket.onopen = () => {
      if (this.closed || socket !== this.socket) {
        socket.close();
        return;
      }
      if (!this.outbound.hasPending) this.reconnectDelay = 250;
      this.onStatus("connected");
      // Announce what we already have; the daemon replies with the difference.
      socket.send(encode(Tag.Subscribe, this.docId, Y.encodeStateVector(this.doc)));
      if (this.hydrated) this.flushOutbound();
    };

    socket.onmessage = (event) => {
      if (this.closed || socket !== this.socket) return;
      const frame = decode(new Uint8Array(event.data as ArrayBuffer));
      if (!frame || frame.docId !== this.docId) return;

      switch (frame.tag) {
        case Tag.Sync:
          this.initialSyncPending = true;
          this.queue(frame.body);
          break;
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
        case Tag.Ack:
          this.acknowledgeUpdate();
          break;
        case Tag.Error:
          console.error("sync error:", new TextDecoder().decode(frame.body));
          // Updates are sent one at a time, so this error belongs to the current
          // head of the queue. Keep it there and ignore later ACKs until a fresh
          // connection retries it.
          this.saveError = true;
          this.setSaveStatus("error");
          // Retry the same queue head on a fresh connection. Persistence
          // errors can be transient, but repeated failures back off so a full
          // disk cannot turn into a tight reconnect loop.
          this.reconnectDelay = Math.max(this.reconnectDelay, 1_000);
          socket.close();
          break;
      }
    };

    socket.onclose = () => {
      if (socket !== this.socket) return;
      const retryingSaveError = this.saveError;
      this.socket = null;
      this.outbound.retryInFlight();
      this.saveError = false;
      if (this.closed) return;
      this.onStatus("offline");
      // An acknowledgement may have been lost with the socket. Resending is
      // safe because Yjs updates are idempotent, and the daemon acknowledges a
      // no-op once it confirms that update is already persisted.
      if (!retryingSaveError) this.setSaveStatus("offline");
      // Back off, but stay responsive: the daemon restarting is a normal event,
      // not an outage.
      this.reconnectTimer = window.setTimeout(() => {
        this.reconnectTimer = null;
        this.connect();
      }, this.reconnectDelay);
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

  withEditorTransaction<T>(ranges: readonly EditorRange[], run: () => T): T {
    if (this.activeEditorTransaction !== null) return run();
    const capture = {
      ranges: ranges.map((range) => ({ ...range })),
      updates: [] as Uint8Array[],
    };
    this.activeEditorTransaction = capture;
    try {
      return run();
    } finally {
      this.activeEditorTransaction = null;
      if (capture.updates.length > 0) {
        const update =
          capture.updates.length === 1 ? capture.updates[0] : Y.mergeUpdates(capture.updates);
        this.enqueueLocal(update, capture.ranges);
      }
    }
  }

  subscribeHydration(listener: (hydrated: boolean) => void): () => void {
    this.hydrationListeners.add(listener);
    listener(this.hydrated);
    return () => this.hydrationListeners.delete(listener);
  }

  /**
   * Observe whether local changes have reached durable storage. The current
   * value is delivered immediately so a toolbar does not need a separate
   * getter or guess at initial state.
   */
  subscribeSaveStatus(listener: (status: SaveStatus) => void): () => void {
    this.saveListeners.add(listener);
    listener(this.saveStatus);
    return () => this.saveListeners.delete(listener);
  }

  /** True until every local update has a durable daemon acknowledgement. */
  get hasPendingChanges(): boolean {
    return this.outbound.hasPending;
  }

  /** True once the first daemon snapshot has replaced the empty editor. */
  get isHydrated(): boolean {
    return this.hydrated;
  }

  /**
   * Wait for pending local work to reach SQLite, without waiting forever when
   * the daemon is unavailable. `false` means the caller must keep this provider
   * alive or ask before navigating away.
   */
  waitUntilSaved(timeoutMs = 2_000): Promise<boolean> {
    if (!this.hasPendingChanges) return Promise.resolve(true);
    if (this.closed) return Promise.resolve(false);

    return new Promise((resolve) => {
      let settled = false;
      let timeout: number | null = null;
      let unsubscribe = () => {};
      const finish = (saved: boolean) => {
        if (settled) return;
        settled = true;
        if (timeout !== null) clearTimeout(timeout);
        unsubscribe();
        resolve(saved);
      };

      unsubscribe = this.subscribeSaveStatus(() => {
        if (!this.hasPendingChanges) finish(true);
        else if (this.closed) finish(false);
      });
      if (!settled) {
        timeout = window.setTimeout(() => finish(false), Math.max(0, timeoutMs));
      }
    });
  }

  destroy() {
    this.closed = true;
    this.doc.off("update", this.onLocalUpdate);
    this.awareness.off("update", this.onAwarenessChange);
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    if (this.timer !== null) clearTimeout(this.timer);
    if (this.reconnectTimer !== null) clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    const socket = this.socket;
    this.socket = null;
    socket?.close();
    // Wake save guards even if the visible state was already Offline.
    this.setSaveStatus(this.hasPendingChanges ? "offline" : "saved", true);
  }

  private onLocalUpdate = (update: Uint8Array, origin: unknown) => {
    if (origin === REMOTE) return;
    if (this.activeEditorTransaction !== null) {
      this.activeEditorTransaction.updates.push(update.slice());
      return;
    }
    this.enqueueLocal(update, []);
  };

  private enqueueLocal(update: Uint8Array, ranges: readonly EditorRange[]) {
    this.outbound.enqueue(update, ranges);
    if (this.socket?.readyState === WebSocket.OPEN) {
      this.flushOutbound();
    } else if (
      this.saveStatus !== "connecting" &&
      this.saveStatus !== "error"
    ) {
      this.setSaveStatus("offline");
    }
  }

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

  private flushOutbound() {
    const socket = this.socket;
    if (
      socket?.readyState !== WebSocket.OPEN ||
      this.saveError ||
      !this.hydrated ||
      !this.outbound.hasPending
    ) {
      return;
    }

    const mutation = this.outbound.beginSend();
    if (mutation === null) return;
    socket.send(encodeEditorMutation(this.docId, mutation.ranges, mutation.update));
    this.setSaveStatus("saving");
  }

  private acknowledgeUpdate() {
    // An ACK after an Error cannot be correlated safely. Freeze until reconnect
    // instead of letting it consume the failed update and shift the FIFO.
    if (this.saveError || !this.outbound.acknowledge()) return;
    this.reconnectDelay = 250;

    if (!this.outbound.hasPending) {
      this.setSaveStatus("saved");
    } else {
      this.flushOutbound();
    }
  }

  private setSaveStatus(status: SaveStatus, force = false) {
    if (!force && status === this.saveStatus) return;
    this.saveStatus = status;
    for (const listener of this.saveListeners) listener(status);
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
    if (this.initialSyncPending) {
      this.initialSyncPending = false;
      this.setHydrated();
    }
  }

  private setHydrated() {
    if (this.hydrated) return;
    this.hydrated = true;
    for (const listener of this.hydrationListeners) listener(true);
    if (this.outbound.hasPending) this.flushOutbound();
    else this.setSaveStatus("saved");
  }

  /** Pending inbound updates. Exposed so tests can observe the buffer. */
  get pending() {
    return this.inbound.length;
  }

  /** Pending outbound queue entries. The in-flight head counts as one. */
  get pendingOutbound() {
    return this.outbound.length;
  }
}
