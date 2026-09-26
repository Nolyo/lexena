// @vitest-environment jsdom
import { describe, it, expect, vi, afterEach } from "vitest";

// Stub localStorage / navigator before any module under test loads, since
// `@/i18n` (transitively imported via useTranslation init) reads them at
// module load time.
vi.hoisted(() => {
  const store = new Map<string, string>();
  const localStorageStub = {
    getItem: (k: string) => (store.has(k) ? store.get(k)! : null),
    setItem: (k: string, v: string) => void store.set(k, String(v)),
    removeItem: (k: string) => void store.delete(k),
    clear: () => store.clear(),
    key: (i: number) => Array.from(store.keys())[i] ?? null,
    get length() {
      return store.size;
    },
  };
  Object.defineProperty(globalThis, "localStorage", {
    value: localStorageStub,
    configurable: true,
    writable: true,
  });
  if (!("navigator" in globalThis) || !globalThis.navigator?.language) {
    Object.defineProperty(globalThis, "navigator", {
      value: { language: "fr-FR" },
      configurable: true,
      writable: true,
    });
  }
});

import { render, screen, cleanup } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

afterEach(() => {
  cleanup();
});

vi.mock("@/hooks/useCloud", () => ({
  useCloud: vi.fn(),
}));

import "@/i18n";
import { ExpirationPopup } from "./ExpirationPopup";
import { useCloud } from "@/hooks/useCloud";

describe("ExpirationPopup", () => {
  it("does not show an expired trial when server-managed cloud access is active", () => {
    vi.mocked(useCloud).mockReturnValue({
      trial: { is_active: false, minutes_remaining: 0, expires_at: "2020-01-01T00:00:00Z" },
      plan: null,
      isCloudEligible: true,
    } as ReturnType<typeof useCloud>);
    const { container } = render(<ExpirationPopup />);
    expect(container).toBeEmptyDOMElement();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("renders nothing when trial is still active", () => {
    (useCloud as ReturnType<typeof vi.fn>).mockReturnValue({
      trial: {
        is_active: true,
        minutes_remaining: 30,
        expires_at: "2099-01-01T00:00:00Z",
      },
      plan: null,
    });
    const { container } = render(<ExpirationPopup />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders trial-expired dialog when trial is over and no plan", () => {
    (useCloud as ReturnType<typeof vi.fn>).mockReturnValue({
      trial: {
        is_active: false,
        minutes_remaining: 0,
        expires_at: "2020-01-01T00:00:00Z",
      },
      plan: null,
    });
    render(<ExpirationPopup />);
    expect(
      screen.getByText(/Ton essai est terminé|Your trial has ended/),
    ).toBeInTheDocument();
  });

  it("renders nothing when active subscription exists", () => {
    (useCloud as ReturnType<typeof vi.fn>).mockReturnValue({
      trial: {
        is_active: false,
        minutes_remaining: 0,
        expires_at: "2020-01-01T00:00:00Z",
      },
      plan: { quota_minutes: 400, plan: "starter" },
    });
    const { container } = render(<ExpirationPopup />);
    expect(container).toBeEmptyDOMElement();
  });
});
