/**
 * The `/` menu.
 *
 * Several nodes were in the schema with no way to reach them — a table could
 * render and be typed into, but nothing could create one. Markdown input rules
 * cover headings, lists, quotes and code as you type; this covers the rest
 * without adding permanent chrome to a window whose whole point is not having
 * any.
 */
import type { Editor } from "@tiptap/core";

type Command = {
  title: string;
  hint: string;
  keywords: string[];
  run: (editor: Editor) => void;
};

const COMMANDS: Command[] = [
  {
    title: "Heading",
    hint: "# ",
    keywords: ["h1", "title"],
    run: (e) => e.chain().focus().toggleHeading({ level: 1 }).run(),
  },
  {
    title: "Subheading",
    hint: "## ",
    keywords: ["h2"],
    run: (e) => e.chain().focus().toggleHeading({ level: 2 }).run(),
  },
  {
    title: "Bulleted list",
    hint: "- ",
    keywords: ["ul", "bullet"],
    run: (e) => e.chain().focus().toggleBulletList().run(),
  },
  {
    title: "Numbered list",
    hint: "1. ",
    keywords: ["ol", "ordered"],
    run: (e) => e.chain().focus().toggleOrderedList().run(),
  },
  {
    title: "Quote",
    hint: "> ",
    keywords: ["blockquote"],
    run: (e) => e.chain().focus().toggleBlockquote().run(),
  },
  {
    title: "Code block",
    hint: "```",
    keywords: ["pre", "snippet"],
    run: (e) => e.chain().focus().toggleCodeBlock().run(),
  },
  {
    title: "Divider",
    hint: "---",
    keywords: ["hr", "rule", "separator"],
    run: (e) => e.chain().focus().setHorizontalRule().run(),
  },
  {
    title: "Table",
    hint: "3 × 3",
    keywords: ["grid", "rows", "columns"],
    // The one with no other way in: GFM tables need a header row, so the
    // inserted table has one.
    run: (e) =>
      e.chain().focus().insertTable({ rows: 3, cols: 3, withHeaderRow: true }).run(),
  },
];

function matches(query: string): Command[] {
  const q = query.trim().toLowerCase();
  if (!q) return COMMANDS;
  return COMMANDS.filter(
    (c) =>
      c.title.toLowerCase().includes(q) || c.keywords.some((k) => k.startsWith(q)),
  );
}

/** Attaches the menu to an editor. Returns a teardown function. */
export function installSlashMenu(editor: Editor, host: HTMLElement): () => void {
  const menu = document.createElement("div");
  menu.className = "slash";
  menu.hidden = true;
  host.append(menu);

  let open = false;
  let query = "";
  let selected = 0;
  let results = COMMANDS;

  function place() {
    // Anchored to the caret, so the menu appears where you are typing rather
    // than in some fixed corner.
    const coords = editor.view.coordsAtPos(editor.state.selection.from);
    menu.style.left = `${coords.left}px`;
    menu.style.top = `${coords.bottom + 6}px`;
  }

  function render() {
    results = matches(query);
    selected = Math.min(selected, Math.max(0, results.length - 1));
    menu.replaceChildren(
      ...results.map((command, i) => {
        const row = document.createElement("div");
        row.className = i === selected ? "slash-item selected" : "slash-item";
        const title = document.createElement("span");
        title.textContent = command.title;
        const hint = document.createElement("span");
        hint.className = "slash-hint";
        hint.textContent = command.hint;
        row.append(title, hint);
        row.addEventListener("mousedown", (event) => {
          event.preventDefault();
          choose(i);
        });
        return row;
      }),
    );
    if (results.length === 0) {
      const none = document.createElement("div");
      none.className = "slash-item none";
      none.textContent = "No matches";
      menu.replaceChildren(none);
    }
  }

  function show() {
    open = true;
    query = "";
    selected = 0;
    menu.hidden = false;
    place();
    render();
  }

  function hide() {
    open = false;
    menu.hidden = true;
  }

  /** Delete the typed `/query`, then run the command in its place. */
  function choose(index: number) {
    const command = results[index];
    if (!command) return;
    const to = editor.state.selection.from;
    const from = to - (query.length + 1);
    hide();
    editor.chain().focus().deleteRange({ from, to }).run();
    command.run(editor);
  }

  const onKeyDown = (event: KeyboardEvent) => {
    if (!open) {
      // Only at the start of an empty block: a slash mid-sentence is a slash.
      if (event.key === "/" && editor.state.selection.empty) {
        const { $from } = editor.state.selection;
        if ($from.parent.textContent.length === 0) setTimeout(show, 0);
      }
      return;
    }

    if (event.key === "Escape") {
      event.preventDefault();
      hide();
    } else if (event.key === "Enter") {
      event.preventDefault();
      choose(selected);
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      selected = Math.max(
        0,
        Math.min(results.length - 1, selected + (event.key === "ArrowDown" ? 1 : -1)),
      );
      render();
    } else if (event.key === "Backspace") {
      if (query.length === 0) hide();
      else {
        query = query.slice(0, -1);
        render();
      }
    } else if (event.key.length === 1) {
      query += event.key;
      render();
    }
  };

  const onBlur = () => hide();
  const dom = editor.view.dom;
  dom.addEventListener("keydown", onKeyDown, true);
  dom.addEventListener("blur", onBlur);

  return () => {
    dom.removeEventListener("keydown", onKeyDown, true);
    dom.removeEventListener("blur", onBlur);
    menu.remove();
  };
}
