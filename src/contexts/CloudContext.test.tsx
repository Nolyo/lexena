// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { useContext } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  user: { id: "owner" } as { id: string } | null,
  provider: "LexenaCloud",
  query: vi.fn(), invoke: vi.fn(async () => {}),
}));
vi.mock("@/hooks/useAuth", () => ({ useAuth: () => ({ user: mocks.user }) }));
vi.mock("@/hooks/useSettings", () => ({ useSettings: () => ({ settings: { transcription_provider: mocks.provider } }) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock("@/lib/supabase", () => ({
  supabase: { from: (table: string) => {
    let id: string;
    const query = {
      select: () => query,
      eq: (key: string, value: string) => { if (key === "user_id") id = value; return query; },
      gte: () => query, lt: () => query,
      maybeSingle: () => mocks.query(table, id),
      then: (resolve: (value: unknown) => void) => Promise.resolve(mocks.query(table, id)).then(resolve),
    };
    return query;
  } },
}));
import { CloudContext, CloudProvider, type CloudContextValue } from "./CloudContext";

const grant = { revoked_at: null, expires_at: null, monthly_minutes_limit: 1000, monthly_tokens_limit: 1_000_000 };
let latest: CloudContextValue;
function Consumer() {
  const value = useContext(CloudContext);
  latest = value;
  return <div data-testid="state">{value.mode}:{String(value.isCloudEligible)}</div>;
}
const App = () => <CloudProvider><Consumer /></CloudProvider>;
beforeEach(() => {
  mocks.user = { id: "owner" };
  mocks.provider = "LexenaCloud";
  mocks.query.mockImplementation(async (table, id) => ({
    data: table === "cloud_access_grants" && id === "owner" ? grant : table === "usage_events" ? [] : null,
    error: null,
  }));
});
afterEach(cleanup);

describe("owner cloud routing", () => {
  it("routes the granted account to cloud without a paid plan or trial", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByTestId("state").textContent).toBe("cloud:true"));
    expect(latest.plan).toBeNull();
    expect(mocks.invoke).toHaveBeenCalledWith("set_cloud_gate", { provider: "LexenaCloud", eligible: true });
  });
  it("honors a deliberate local provider choice", async () => {
    mocks.provider = "Local";
    render(<App />);
    await waitFor(() => expect(screen.getByTestId("state").textContent).toBe("local:true"));
  });
  it("clears the grant when a refresh fails", async () => {
    render(<App />);
    await waitFor(() => expect(screen.getByTestId("state").textContent).toBe("cloud:true"));
    mocks.query.mockResolvedValue({ data: null, error: { message: "unavailable" } });
    await act(() => latest.refreshUsage());
    expect(screen.getByTestId("state").textContent).toBe("local:false");
  });
  it("excludes offered minutes from the paid usage display after revocation", async () => {
    mocks.query.mockImplementation(async (table) => ({
      data: table === "subscriptions" ? { status: "active", plan: "starter", quota_minutes: 400 }
        : table === "usage_summary" ? { units_total: 1050, complimentary_units_total: 1000 }
        : table === "usage_events" ? [] : null,
      error: null,
    }));
    render(<App />);
    await waitFor(() => expect(screen.getByTestId("state").textContent).toBe("cloud:true"));
    expect(latest.monthly_minutes_used).toBe(50);
    expect(latest.ownerAccess).toBeNull();
  });
  it("cannot restore the previous account's grant from a delayed response", async () => {
    const view = render(<App />);
    await waitFor(() => expect(screen.getByTestId("state").textContent).toBe("cloud:true"));
    let resolveGrant!: (result: unknown) => void;
    mocks.query.mockImplementation((table, id) => {
      if (table === "cloud_access_grants" && id === "owner") return new Promise(resolve => { resolveGrant = resolve; });
      return Promise.resolve({ data: table === "usage_events" ? [] : null, error: null });
    });
    let refresh!: Promise<void>;
    act(() => { refresh = latest.refreshUsage(); });
    mocks.user = { id: "another-user" };
    view.rerender(<App />);
    await waitFor(() => expect(screen.getByTestId("state").textContent).toBe("local:false"));
    await act(async () => { resolveGrant({ data: grant, error: null }); await refresh; });
    expect(screen.getByTestId("state").textContent).toBe("local:false");
  });
});
