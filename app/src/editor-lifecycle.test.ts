import { describe, expect, it, vi } from "vitest";
import {
  canExportEditor,
  lockEditorInteraction,
  runFrozenDestructiveAction,
  syncEditorReadiness,
  type EditorReadinessTarget,
} from "./editor-lifecycle";

function readinessTarget(hydrated: boolean, editable = hydrated) {
  const state = { hydrated, editable };
  const target = {
    editor: {
      get isEditable() {
        return state.editable;
      },
      setEditable(value: boolean) {
        state.editable = value;
      },
    },
    provider: {
      get isHydrated() {
        return state.hydrated;
      },
    },
  } satisfies EditorReadinessTarget;
  return { state, target };
}

describe("editor readiness", () => {
  it("blocks export and editing until hydration", () => {
    const { state, target } = readinessTarget(false);
    expect(canExportEditor(target)).toBe(false);
    expect(syncEditorReadiness(target)).toBe(false);

    state.hydrated = true;
    expect(canExportEditor(target)).toBe(true);
    expect(syncEditorReadiness(target)).toBe(true);
    expect(state.editable).toBe(true);
  });

  it("keeps nested locks closed and uses current hydration on final release", () => {
    const { state, target } = readinessTarget(false);
    const releaseTrash = lockEditorInteraction(target);
    const releaseClose = lockEditorInteraction(target);

    state.hydrated = true;
    syncEditorReadiness(target);
    expect(state.editable).toBe(false);
    releaseClose();
    expect(state.editable).toBe(false);
    releaseTrash();
    expect(state.editable).toBe(true);
  });
});

describe("frozen destructive actions", () => {
  it("stays frozen through follow-up work and destroys only after commit", async () => {
    const { state, target } = readinessTarget(true);
    const order: string[] = [];
    let finishMutation = () => {};
    const mutation = new Promise<void>((resolve) => {
      finishMutation = resolve;
    });
    const destroyed = vi.fn(() => order.push("destroy"));
    const action = runFrozenDestructiveAction(
      target,
      () => true,
      () => mutation,
      async () => {
        order.push(`after:${state.editable}`);
        return "next";
      },
      destroyed,
    );

    expect(state.editable).toBe(false);
    finishMutation();
    await expect(action).resolves.toBe("next");
    expect(order).toEqual(["after:false", "destroy"]);
    expect(destroyed).toHaveBeenCalledOnce();
  });

  it("restores from current hydration when deletion fails", async () => {
    const { state, target } = readinessTarget(false);
    let rejectMutation = (_error: Error) => {};
    const mutation = new Promise<void>((_resolve, reject) => {
      rejectMutation = reject;
    });
    const action = runFrozenDestructiveAction(
      target,
      () => true,
      () => mutation,
      async () => null,
      vi.fn(),
    );

    state.hydrated = true;
    syncEditorReadiness(target);
    expect(state.editable).toBe(false);
    rejectMutation(new Error("delete failed"));
    await expect(action).rejects.toThrow("delete failed");
    expect(state.editable).toBe(true);
  });

  it("does not destroy a replacement editor after navigation", async () => {
    const { target } = readinessTarget(true);
    let current = true;
    const destroyed = vi.fn();
    await runFrozenDestructiveAction(
      target,
      () => current,
      async () => {},
      async () => {
        current = false;
      },
      destroyed,
    );
    expect(destroyed).not.toHaveBeenCalled();
  });
});
