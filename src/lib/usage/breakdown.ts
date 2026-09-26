export interface UsageEventRow {
  source: "trial" | "quota" | "overage" | "complimentary";
  units: number;
}

export interface MonthlyBreakdown {
  trial: number;
  quota: number;
  overage: number;
}

export function computeBreakdown(rows: UsageEventRow[]): MonthlyBreakdown {
  const acc: MonthlyBreakdown = { trial: 0, quota: 0, overage: 0 };
  for (const row of rows) {
    if (row.source === "trial" || row.source === "quota" || row.source === "overage") {
      acc[row.source] += Number(row.units);
    }
  }
  return acc;
}
