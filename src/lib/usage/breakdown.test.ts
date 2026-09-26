import { describe, it, expect } from "vitest";
import { computeBreakdown, type UsageEventRow } from "./breakdown";

describe("computeBreakdown", () => {
  it("keeps complimentary usage out of the paid and trial breakdown", () => {
    expect(computeBreakdown([
      { source: "complimentary", units: 1000 },
      { source: "quota", units: 3 },
    ])).toEqual({ trial: 0, quota: 3, overage: 0 });
  });
  it("returns zeros for an empty list", () => {
    expect(computeBreakdown([])).toEqual({ trial: 0, quota: 0, overage: 0 });
  });

  it("sums units per source", () => {
    const rows: UsageEventRow[] = [
      { source: "trial", units: 1.5 },
      { source: "trial", units: 2.25 },
      { source: "quota", units: 10 },
      { source: "overage", units: 3 },
    ];
    expect(computeBreakdown(rows)).toEqual({ trial: 3.75, quota: 10, overage: 3 });
  });

  it("ignores unknown sources (forward-compat)", () => {
    const rows = [
      { source: "trial", units: 1 },
      { source: "future_source" as unknown as UsageEventRow["source"], units: 5 },
    ];
    expect(computeBreakdown(rows as UsageEventRow[])).toEqual({
      trial: 1,
      quota: 0,
      overage: 0,
    });
  });

  it("counts zero-unit events", () => {
    expect(computeBreakdown([{ source: "trial", units: 0 }])).toEqual({
      trial: 0,
      quota: 0,
      overage: 0,
    });
  });

  it("coerces units to number when DB returns string (numeric)", () => {
    const rows = [
      { source: "quota", units: "2.50" as unknown as number },
      { source: "quota", units: 1.5 },
    ];
    expect(computeBreakdown(rows as UsageEventRow[])).toEqual({
      trial: 0,
      quota: 4,
      overage: 0,
    });
  });
});
