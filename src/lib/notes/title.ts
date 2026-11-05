const LINE_BREAK_ELEMENTS = new Set([
  "p", "h1", "h2", "h3", "h4", "h5", "h6", "br", "li", "tr", "blockquote", "pre",
]);
const OMITTED_ELEMENTS = new Set(["script", "style", "iframe", "object"]);

/** Mirrors Rust's first-visible-line rule; parsing stays in an inert template. */
export function deriveNoteTitle(html: string, emptyTitle: string): string {
  const template = document.createElement("template");
  template.innerHTML = html;
  let text = "";
  function visit(node: Node): void {
    if (node.nodeType === 3) {
      text += node.textContent ?? "";
      return;
    }
    if (node.nodeType === 1) {
      const tag = (node as Element).tagName.toLowerCase();
      if (OMITTED_ELEMENTS.has(tag)) return;
      if (tag === "img") { text += "[image]"; return; }
      if (LINE_BREAK_ELEMENTS.has(tag) && text && !text.endsWith("\n")) text += "\n";
    }
    node.childNodes.forEach(visit);
  }
  visit(template.content);
  const first = text.split(/\r?\n/).map((line) => line.trim()).find(Boolean);
  if (!first) return emptyTitle;
  if (first.length <= 50) return first;
  let title = "";
  for (const character of first) {
    if (title.length + character.length > 50) break;
    title += character;
  }
  return `${title}...`;
}
