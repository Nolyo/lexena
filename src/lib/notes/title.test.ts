// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { describe, expect, it, vi } from "vitest";
const locale = vi.hoisted(() => ({ value: "en" }));
vi.mock("@/i18n", () => ({ default: {
  get language() { return locale.value; },
  t: () => locale.value === "fr" ? "Note sans titre" : "Untitled note",
} }));
import { deriveTitle } from "@/hooks/useNotes";

const fixtures = JSON.parse(readFileSync("src-tauri/tests/fixtures/mcp-notes/titles.json", "utf8")) as Array<{ html: string; locale: string; title: string }>;

describe("note title parity with the MCP content service", () => {
  for (const fixture of fixtures) {
    it(`preserves the first visible line for ${JSON.stringify(fixture.html)}`, async () => {
      locale.value = fixture.locale;
      expect(deriveTitle(fixture.html)).toBe(fixture.title);
    });
  }

  it("does not split an emoji at the fifty UTF-16 unit boundary", () => {
    expect(deriveTitle(`<h1>${"a".repeat(49)}😀x</h1>`)).toBe(`${"a".repeat(49)}...`);
  });
});
