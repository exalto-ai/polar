/** Shared readiness and temporary interaction locks for one editor replica. */

export type EditorReadinessTarget = {
  editor: {
    readonly isEditable: boolean;
    setEditable: (editable: boolean) => void;
  };
  provider: { readonly isHydrated: boolean };
};

const interactionLocks = new WeakMap<object, number>();

function lockCount(target: EditorReadinessTarget): number {
  return interactionLocks.get(target.editor) ?? 0;
}

/** Apply the current hydration and interaction-lock state to the editor. */
export function syncEditorReadiness(target: EditorReadinessTarget): boolean {
  const editable = target.provider.isHydrated && lockCount(target) === 0;
  target.editor.setEditable(editable);
  return editable;
}

/** Export must never project TipTap's empty pre-hydration placeholder tree. */
export function canExportEditor(
  target: EditorReadinessTarget | null,
): target is EditorReadinessTarget {
  return target?.provider.isHydrated === true;
}

/**
 * Add one composable interaction lock. Releasing the last lock derives the
 * editor state from current hydration, so a Sync received while locked is not
 * lost and one nested operation cannot unlock another.
 */
export function lockEditorInteraction(
  target: EditorReadinessTarget,
): (restore?: boolean) => void {
  interactionLocks.set(target.editor, lockCount(target) + 1);
  syncEditorReadiness(target);
  let released = false;
  return (restore = true) => {
    if (released) return;
    released = true;
    const remaining = Math.max(0, lockCount(target) - 1);
    if (remaining === 0) interactionLocks.delete(target.editor);
    else interactionLocks.set(target.editor, remaining);
    if (restore) syncEditorReadiness(target);
  };
}

/**
 * Keep an active editor frozen across a destructive request and its follow-up
 * reads. A failed request restores current readiness. Once the request commits,
 * the original editor is destroyed if it is still current and is never briefly
 * re-enabled as a tombstone.
 */
export async function runFrozenDestructiveAction<T>(
  target: EditorReadinessTarget,
  isCurrent: () => boolean,
  mutate: () => Promise<void>,
  afterMutation: () => Promise<T>,
  destroyCurrent: () => void,
): Promise<T> {
  const release = lockEditorInteraction(target);
  try {
    await mutate();
  } catch (error) {
    release(isCurrent());
    throw error;
  }

  try {
    return await afterMutation();
  } finally {
    const current = isCurrent();
    release(false);
    if (current) destroyCurrent();
  }
}
