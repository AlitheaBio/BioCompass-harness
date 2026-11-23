const FORMULA_TRIGGER_RE = /^[=+\-@\t\r]/;
const FINITE_NUMBER_RE = /^[+-]?(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$/;

/**
 * Apply the `[R7]` non-numeric CSV/TSV formula guard (`SEC-30`).
 * Preserves signed numbers (`-0.73`, `+3`, `-2e-4`) and numeric primitives byte-for-byte
 * while prefixing non-numeric strings starting with `=`, `+`, `-`, `@`, `\t`, or `\r` with `'`.
 */
export function guardCsvFormula(val: unknown): string {
  if (val == null) return "";
  if (typeof val === "number" || typeof val === "boolean") {
    return String(val);
  }
  const s = String(val);
  if (!s || !FORMULA_TRIGGER_RE.test(s)) {
    return s;
  }
  const trimmed = s.trim();
  if (FINITE_NUMBER_RE.test(trimmed) && Number.isFinite(Number(trimmed))) {
    return s;
  }
  return `'${s}`;
}

export function serializeRowsForExport(
  rows: Record<string, unknown>[],
  cols: Array<{ key: string }>,
  format: "csv" | "tsv" | "json",
): string {
  if (format === "json") {
    return JSON.stringify(rows, null, 2);
  }
  const sep = format === "tsv" ? "\t" : ",";
  const escapeCell = (val: unknown): string => {
    const guarded = guardCsvFormula(val);
    if (format === "csv") {
      if (
        guarded.includes(",") ||
        guarded.includes('"') ||
        guarded.includes("\n") ||
        guarded.includes("\r")
      ) {
        return `"${guarded.replace(/"/g, '""')}"`;
      }
      return guarded;
    }
    // TSV: neutralize raw tabs/newlines AND escape double quotes cleanly (`SEC-30`).
    const flat = guarded.replace(/[\r\n\t]+/g, " ");
    if (flat.includes('"')) {
      return `"${flat.replace(/"/g, '""')}"`;
    }
    return flat;
  };
  const header = cols.map((c) => escapeCell(c.key)).join(sep);
  const lines = rows.map((row) =>
    cols.map((c) => escapeCell(row[c.key])).join(sep),
  );
  return [header, ...lines].join("\n");
}
