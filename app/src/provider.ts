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
  encodeSourcedUpdate,
  LocalInputSource,
  Tag,
  type LocalInputSource as LocalInputSourceValue,
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

type OutboundUpdate = {
  source: LocalInputSourceValue;
  update: Uint8Array;
};

/**
 * Acknowledgements are positional, so the update already sent must remain
 * unchanged until its ACK arrives. Unsent updates may coalesce only when their
 * sources match and they are adjacent. Crossing a written/paste boundary would
 * permanently erase the distinction because Yjs updates carry no origin.
 */
class OutboundQueue {
  private inFlight: OutboundUpdate | null = null;
  private queued: OutboundUpdate[] = [];

  get length(): number {
    return this.queued.length + (this.inFlight === null ? 0 : 1);
  }

  get hasPending(): boolean {
    return this.length > 0;
  }

  enqueue(update: Uint8Array, source: LocalInputSourceValue) {
    const incoming = { source, update: update.slice() };
    const tail = this.queued[this.queued.length - 1];
    if (tail?.source === incoming.source) {
      tail.update = Y.mergeUpdates([tail.update, incoming.update]);
    } else {
      this.queued.push(incoming);
    }
  }

  /** Move the oldest unsent source run into the in-flight position. */
  beginSend(): OutboundUpdate | null {
    if (this.inFlight !== null) return null;
    this.inFlight = this.queued.shift() ?? null;
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
   * queued work are both unsent for the next connection. They become one run
   * only if their adjacent source labels agree.
   */
  retryInFlight() {
    if (this.inFlight === null) return;
    const head = this.inFlight;
    this.inFlight = null;
    const next = this.queued[0];
    if (next?.source === head.source) {
      next.update = Y.mergeUpdates([head.update, next.update]);
    } else {
      this.queued.unshift(head);
    }
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
  /** Source scoped around a TipTap dispatch; Yjs emits its update synchronously. */
  private activeLocalSource: LocalInputSourceValue | null = null;
  /**
   * A DOM event can cause Yjs work without a TipTap dispatch, notably undo.
   * Keep the observation for the current browser task as a narrow fallback.
   */
  private pendingLocalSource: LocalInputSourceValue | null = null;
  private pendingSourceGeneration = 0;

  constructor(
    private readonly url: string,
    private readonly editorToken: string,
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
    // A new connection returns the in-flight head to the source-run queue.
    // Adjacent runs with the same source may merge, but distinct source
    // boundaries remain separate. Resending is safe when only the ACK was lost
    // because Yjs updates are idempotent.
    this.outbound.retryInFlight();
    this.saveError = false;
    // The browser WebSocket API cannot set headers, so the editor capability
    // rides as a subprotocol. It is header-borne and never part of the URL,
    // which would otherwise put a bearer credential into logs and history.
    const socket = new WebSocket(this.url, [
      "thought.v1",
      `thought.token.${this.editorToken}`,
    ]);
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
      this.flushOutbound();
      if (!this.outbound.hasPending) this.setSaveStatus("saved");
    };

    socket.onmessage = (event) => {
      if (this.closed || socket !== this.socket) return;
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

  /**
   * Scope the next synchronous Yjs update to the transaction that produced it.
   * Nested scopes restore the outer source, which keeps plugin composition
   * deterministic.
   */
  withLocalInputSource<T>(source: LocalInputSourceValue, run: () => T): T {
    const previous = this.activeLocalSource;
    this.activeLocalSource = source;
    try {
      return run();
    } finally {
      this.activeLocalSource = previous;
    }
  }

  /**
   * Remember an observed editor event for direct Yjs commands and delayed DOM
   * reconciliation. A later browser task must not inherit a stale source.
   */
  noteLocalInputSource(source: LocalInputSourceValue) {
    this.pendingLocalSource = source;
    const generation = ++this.pendingSourceGeneration;
    window.setTimeout(() => {
      if (this.pendingSourceGeneration === generation) {
        this.pendingLocalSource = null;
      }
    }, 0);
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
    const source =
      this.activeLocalSource ?? this.pendingLocalSource ?? LocalInputSource.Unknown;
    this.pendingLocalSource = null;
    this.pendingSourceGeneration += 1;
    this.outbound.enqueue(update, source);
    if (this.socket?.readyState === WebSocket.OPEN) {
      this.flushOutbound();
    } else if (
      this.saveStatus !== "connecting" &&
      this.saveStatus !== "error"
    ) {
      this.setSaveStatus("offline");
    }
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

  private flushOutbound() {
    const socket = this.socket;
    if (
      socket?.readyState !== WebSocket.OPEN ||
      this.saveError ||
      !this.outbound.hasPending
    ) {
      return;
    }

    const update = this.outbound.beginSend();
    if (update === null) return;
    socket.send(encodeSourcedUpdate(this.docId, update.source, update.update));
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
