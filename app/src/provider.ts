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
  encodeAnchoredBatch,
  LocalInputSource,
  MAX_ANCHORED_MUTATIONS,
  Tag,
  type AnchoredMutation,
  type AnchoredRangeHint,
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

type OutboundMutation = AnchoredMutation & {
  kind: "update";
};

type OutboundSnapshot = {
  kind: "snapshot";
  clientEventId: string;
};

type OutboundWork = OutboundMutation | OutboundSnapshot;

/** Strict bounds for semantic mutations and update bytes retained by the outbox. */
export const MAX_PENDING_OUTBOUND_RUNS = MAX_ANCHORED_MUTATIONS * 2;
export const MAX_PENDING_OUTBOUND_BYTES = 256 * 1024;

/**
 * Acknowledgements are positional, so the exact batch already sent remains
 * unchanged until its ACK arrives. New work waits behind it and is grouped in
 * ordered batches of at most 128 semantic editor dispatches.
 */
class OutboundQueue {
  private inFlight: readonly OutboundWork[] | null = null;
  private retryPending = false;
  private queued: OutboundWork[] = [];

  get length(): number {
    return this.queued.length + (this.inFlight?.length ?? 0);
  }

  get hasPending(): boolean {
    return this.length > 0;
  }

  get bytes(): number {
    return this.batchBytes(this.inFlight) + this.batchBytes(this.queued);
  }

  enqueue(
    update: Uint8Array,
    source: LocalInputSourceValue,
    hints: readonly AnchoredRangeHint[],
  ) {
    // A queued snapshot is generated from the live Y.Doc only when it is sent,
    // so it already covers every later edit without retaining another update.
    if (this.queued.some((item) => item.kind === "snapshot")) return;

    // The in-flight snapshot represents only the state at its send boundary.
    // One queued snapshot captures edits made while that ACK is outstanding.
    if (this.inFlight?.some((item) => item.kind === "snapshot")) {
      this.queued = [this.snapshotWork()];
      return;
    }

    if (
      this.length + 1 > MAX_PENDING_OUTBOUND_RUNS ||
      this.bytes + update.byteLength > MAX_PENDING_OUTBOUND_BYTES
    ) {
      this.collapseQueuedToSnapshot();
      return;
    }
    this.queued.push({
      kind: "update",
      source,
      clientEventId: randomClientEventId(),
      hints: hints.map((hint) => ({ ...hint })),
      update: update.slice(),
    });
  }

  /** Select the oldest batch, or return the exact batch marked for retry. */
  beginSend(doc: Y.Doc): readonly AnchoredMutation[] | null {
    if (this.inFlight !== null) {
      if (!this.retryPending) return null;
      this.retryPending = false;
      return this.materialize(this.inFlight, doc);
    }
    if (this.queued.length === 0) return null;
    this.inFlight = this.queued.splice(0, MAX_ANCHORED_MUTATIONS);
    return this.materialize(this.inFlight, doc);
  }

  /** Consume exactly the head that the peer has durably acknowledged. */
  acknowledge(): boolean {
    if (this.inFlight === null) return false;
    this.inFlight = null;
    this.retryPending = false;
    return true;
  }

  /**
   * Once the socket is gone, no ACK from it can be accepted. Retry the same
   * immutable batch first, with the same event IDs, hints, ordering, and bytes.
   */
  retryInFlight() {
    if (this.inFlight === null) return;
    if (this.inFlight.some((item) => item.kind === "snapshot")) {
      // An oversized snapshot was deliberately not retained. A retry gets a
      // new event ID so a changed current document cannot conflict with a
      // snapshot the daemon may already have committed under the old ID.
      this.inFlight = null;
      this.collapseQueuedToSnapshot();
      return;
    }
    this.retryPending = true;
  }

  private collapseQueuedToSnapshot() {
    this.queued = [this.snapshotWork()];
  }

  private snapshotWork(): OutboundSnapshot {
    return { kind: "snapshot", clientEventId: randomClientEventId() };
  }

  private materialize(
    work: readonly OutboundWork[],
    doc: Y.Doc,
  ): readonly AnchoredMutation[] {
    if (work.length === 1 && work[0].kind === "snapshot") {
      const snapshot = work[0];
      const mutation: AnchoredMutation = {
        source: LocalInputSource.Unknown,
        clientEventId: snapshot.clientEventId,
        hints: [],
        update: Y.encodeStateAsUpdate(doc),
      };
      if (this.bytes + mutation.update.byteLength <= MAX_PENDING_OUTBOUND_BYTES) {
        this.inFlight = [{ kind: "update", ...mutation }];
      }
      return [mutation];
    }
    return work.map((item) => {
      if (item.kind === "snapshot") {
        throw new Error("a snapshot must be the only outbound mutation");
      }
      return {
        source: item.source,
        clientEventId: item.clientEventId,
        hints: item.hints,
        update: item.update,
      };
    });
  }

  private workBytes(item: OutboundWork): number {
    return item.kind === "update" ? item.update.byteLength : 0;
  }

  private batchBytes(items: readonly OutboundWork[] | null): number {
    return items?.reduce((sum, item) => sum + this.workBytes(item), 0) ?? 0;
  }
}

function randomClientEventId(): string {
  const cryptoApi = globalThis.crypto;
  if (typeof cryptoApi?.randomUUID === "function") return cryptoApi.randomUUID();

  const bytes = new Uint8Array(16);
  if (typeof cryptoApi?.getRandomValues === "function") {
    cryptoApi.getRandomValues(bytes);
  } else {
    for (let index = 0; index < bytes.length; index++) {
      bytes[index] = Math.floor(Math.random() * 256);
    }
  }
  // A compact 128-bit identifier stays comfortably inside the wire's 64-byte
  // UTF-8 limit even in runtimes without randomUUID().
  return [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

type LocalTransactionCapture = {
  source: LocalInputSourceValue;
  hints: readonly AnchoredRangeHint[];
  updates: Uint8Array[];
};

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
  /** No local mutation may leave until the daemon's first snapshot is applied. */
  private hydrated = false;
  private initialSyncPending = false;
  private readonly hydrationListeners = new Set<(hydrated: boolean) => void>();
  /** Yjs updates emitted synchronously inside the current TipTap dispatch. */
  private activeLocalTransaction: LocalTransactionCapture | null = null;
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
    // A new connection makes the exact in-flight batch eligible for retry.
    // Later transactions remain queued behind it. Stable client event IDs make
    // a lost acknowledgement safe even after the daemon committed the batch.
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
      if (this.hydrated) {
        this.flushOutbound();
        if (!this.outbound.hasPending) this.setSaveStatus("saved");
      }
    };

    socket.onmessage = (event) => {
      if (this.closed || socket !== this.socket) return;
      const frame = decode(new Uint8Array(event.data as ArrayBuffer));
      if (!frame || frame.docId !== this.docId) return;

      switch (frame.tag) {
        case Tag.Sync:
          if (!this.hydrated) this.initialSyncPending = true;
          this.queue(frame.body);
          // The first snapshot is the editing barrier, not part of the normal
          // display coalescer. Apply it before any UI can become editable.
          if (!this.hydrated) this.flush();
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
          // Only one batch is in flight, so this error belongs to that exact
          // head. Keep it there and ignore later ACKs until a fresh connection
          // retries it.
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
   * Capture every Yjs update emitted inside one TipTap dispatch and enqueue
   * them as one semantic mutation. y-prosemirror can emit more than once while
   * reconciling a transaction, but those internal updates must not invent new
   * semantic editor-dispatch boundaries.
   */
  withLocalTransaction<T>(
    source: LocalInputSourceValue,
    hints: readonly AnchoredRangeHint[],
    run: () => T,
  ): T {
    const previous = this.activeLocalTransaction;
    const capture: LocalTransactionCapture = { source, hints, updates: [] };
    this.activeLocalTransaction = capture;
    try {
      return run();
    } finally {
      this.activeLocalTransaction = previous;
      if (capture.updates.length > 0) {
        const update =
          capture.updates.length === 1
            ? capture.updates[0]
            : Y.mergeUpdates(capture.updates);
        this.enqueueLocalMutation(update, capture.source, capture.hints);
      }
    }
  }

  /** Compatibility for callers that can classify source but have no ranges. */
  withLocalInputSource<T>(source: LocalInputSourceValue, run: () => T): T {
    return this.withLocalTransaction(source, [], run);
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

  /** Whether the daemon's initial Sync snapshot has been applied to this Y.Doc. */
  get isHydrated(): boolean {
    return this.hydrated;
  }

  /** Observe the initial-sync editing barrier. The current value is immediate. */
  subscribeHydration(listener: (hydrated: boolean) => void): () => void {
    this.hydrationListeners.add(listener);
    listener(this.hydrated);
    return () => this.hydrationListeners.delete(listener);
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
    this.hydrationListeners.clear();
    const socket = this.socket;
    this.socket = null;
    socket?.close();
    // Wake save guards even if the visible state was already Offline.
    this.setSaveStatus(this.hasPendingChanges ? "offline" : "saved", true);
  }

  private onLocalUpdate = (update: Uint8Array, origin: unknown) => {
    if (origin === REMOTE) return;
    const capture = this.activeLocalTransaction;
    if (capture !== null) {
      capture.updates.push(update.slice());
      this.pendingLocalSource = null;
      this.pendingSourceGeneration += 1;
      return;
    }

    const source = this.pendingLocalSource ?? LocalInputSource.Unknown;
    this.pendingLocalSource = null;
    this.pendingSourceGeneration += 1;
    this.enqueueLocalMutation(update, source, []);
  };

  private enqueueLocalMutation(
    update: Uint8Array,
    source: LocalInputSourceValue,
    hints: readonly AnchoredRangeHint[],
  ) {
    this.outbound.enqueue(update, source, hints);
    if (this.hydrated && this.socket?.readyState === WebSocket.OPEN) {
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
      !this.hydrated ||
      this.saveError ||
      !this.outbound.hasPending
    ) {
      return;
    }

    const mutations = this.outbound.beginSend(this.doc);
    if (mutations === null) return;
    socket.send(encodeAnchoredBatch(this.docId, mutations));
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
    const completesInitialSync = this.initialSyncPending;
    const merged = this.inbound.length === 1 ? this.inbound[0] : Y.mergeUpdates(this.inbound);
    this.inbound = [];
    Y.applyUpdate(this.doc, merged, REMOTE);
    if (completesInitialSync && !this.hydrated) {
      this.initialSyncPending = false;
      this.hydrated = true;
      for (const listener of this.hydrationListeners) listener(true);
      this.flushOutbound();
      if (!this.outbound.hasPending) this.setSaveStatus("saved");
    }
  }

  /** Pending inbound updates. Exposed so tests can observe the buffer. */
  get pending() {
    return this.inbound.length;
  }

  /** Pending outbound queue entries. The in-flight head counts as one. */
  get pendingOutbound() {
    return this.outbound.length;
  }

  /** Source-labelled update bytes retained by the bounded outbound queue. */
  get pendingOutboundBytes() {
    return this.outbound.bytes;
  }
}
