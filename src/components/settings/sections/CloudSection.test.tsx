// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.hoisted(() => {
  const store = new Map<string, string>();
  Object.defineProperty(globalThis, "localStorage", {
    value: {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, String(v)),
      removeItem: (k: string) => void store.delete(k),
      clear: () => store.clear(),
      key: (i: number) => Array.from(store.keys())[i] ?? null,
      get length() {
        return store.size;
      },
    },
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

import i18n from "@/i18n";
void i18n.changeLanguage("fr");
import { render, screen, cleanup } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";
import { CloudSection } from "./CloudSection";
import type { TrialStatus, UsagePlan } from "@/contexts/CloudContext";
import type { MonthlyBreakdown } from "@/lib/usage/breakdown";
import type { CloudAccessGrant } from "@/lib/cloud/access";

const mockUseUsage = vi.fn();
const mockUseAuth = vi.fn();

vi.mock("@/hooks/useUsage", () => ({
  useUsage: () => mockUseUsage(),
}));
vi.mock("@/hooks/useAuth", () => ({
  useAuth: () => mockUseAuth(),
}));
vi.mock("@/components/billing/SubscribeButton", () => ({
  SubscribeButton: () => <button data-testid="subscribe-button">subscribe</button>,
}));

interface UsageState {
  trial: TrialStatus;
  monthly_minutes_used: number;
  monthly_minutes_breakdown: MonthlyBreakdown;
  plan: UsagePlan | null;
  ownerAccess: CloudAccessGrant | null;
  loading: boolean;
  refresh: () => Promise<void>;
}

function setUsage(state: Partial<UsageState>) {
  mockUseUsage.mockReturnValue({
    trial: { is_active: false, minutes_remaining: 0, expires_at: null },
    monthly_minutes_used: 0,
    monthly_minutes_breakdown: { trial: 0, quota: 0, overage: 0 },
    plan: null,
    loading: false,
    refresh: vi.fn(),
    ...state,
  });
}

beforeEach(() => {
  mockUseAuth.mockReturnValue({ user: { id: "u1" } });
});

afterEach(() => {
  cleanup();
  mockUseUsage.mockReset();
});

describe("CloudSection", () => {
  it("shows owner access and allowance without an expired-trial or payment prompt", () => {
    setUsage({
      ownerAccess: { monthly_minutes_limit: 1000, monthly_tokens_limit: 1000000, expires_at: null },
      monthly_minutes_used: 12,
    });
    render(<CloudSection />);
    expect(screen.getByText("Accès propriétaire")).toBeInTheDocument();
    expect(screen.getByText("12 / 1000 minutes ce mois-ci")).toBeInTheDocument();
    expect(screen.queryByText(/aucun essai ni abonnement actif/i)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "subscribe" })).not.toBeInTheDocument();
  });

  it("shows signin_required when no user", () => {
    mockUseAuth.mockReturnValue({ user: null });
    setUsage({});
    render(<CloudSection />);
    expect(screen.getByText(/connectez-vous/i)).toBeInTheDocument();
  });

  it("shows loading state", () => {
    setUsage({ loading: true });
    render(<CloudSection />);
    expect(screen.getByText(/chargement/i)).toBeInTheDocument();
  });

  it("renders Plan block with bonus subblock when both trial and plan are active", () => {
    setUsage({
      trial: {
        is_active: true,
        minutes_remaining: 49,
        expires_at: "2026-06-03T19:57:01Z",
      },
      plan: { plan: "starter", quota_minutes: 400 },
      monthly_minutes_used: 5,
      monthly_minutes_breakdown: { trial: 5, quota: 0, overage: 0 },
    });
    render(<CloudSection />);

    // Plan block headline + counter use breakdown.quota (0), NOT monthly_minutes_used (5)
    expect(screen.getByText(/plan starter/i)).toBeInTheDocument();
    expect(screen.getByText("0 / 400")).toBeInTheDocument();

    // Bonus subblock visible with explicit ordering note
    expect(screen.getByText(/bonus de bienvenue/i)).toBeInTheDocument();
    expect(screen.getByText(/49 min restantes/i)).toBeInTheDocument();
    expect(screen.getByText(/consommées en priorité avant ton plan/i)).toBeInTheDocument();
  });

  it("renders Plan block alone when trial is expired", () => {
    setUsage({
      trial: { is_active: false, minutes_remaining: 0, expires_at: null },
      plan: { plan: "starter", quota_minutes: 400 },
      monthly_minutes_breakdown: { trial: 0, quota: 27, overage: 0 },
    });
    render(<CloudSection />);
    expect(screen.getByText(/plan starter/i)).toBeInTheDocument();
    expect(screen.getByText("27 / 400")).toBeInTheDocument();
    expect(screen.queryByText(/bonus de bienvenue/i)).not.toBeInTheDocument();
  });

  it("renders bonus block + Subscribe CTA when trial active and no plan", () => {
    setUsage({
      trial: {
        is_active: true,
        minutes_remaining: 49,
        expires_at: "2026-06-03T19:57:01Z",
      },
      plan: null,
    });
    render(<CloudSection />);
    expect(screen.getByText(/bonus de bienvenue/i)).toBeInTheDocument();
    expect(screen.queryByText(/consommées en priorité/i)).not.toBeInTheDocument();
    expect(screen.getByTestId("subscribe-button")).toBeInTheDocument();
  });

  it("shows nothing_active when neither trial nor plan", () => {
    setUsage({});
    render(<CloudSection />);
    expect(screen.getByText(/aucun essai ni abonnement actif/i)).toBeInTheDocument();
    expect(screen.getByTestId("subscribe-button")).toBeInTheDocument();
  });
});
