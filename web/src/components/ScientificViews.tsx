import type { DeclarativeViewSpec } from "../types";

function resolveJsonPointer(root: Record<string, unknown>, pointer: string): unknown {
  if (!pointer || pointer === "/") return root;
  const parts = pointer
    .split("/")
    .slice(1)
    .map((seg) => seg.replace(/~1/g, "/").replace(/~0/g, "~"));
  let cur: unknown = root;
  for (const part of parts) {
    if (typeof cur !== "object" || cur === null || !(part in cur)) {
      return undefined;
    }
    cur = (cur as Record<string, unknown>)[part];
  }
  return cur;
}

interface MetricCardItem {
  label: string;
  value: string | number;
}

interface BarItem {
  label?: string;
  allele?: string;
  length?: number;
  value?: number;
  count?: number;
  strong_binders?: number;
}

function getBarLabel(item: BarItem): string {
  if (typeof item.label === "string") return item.label;
  if (typeof item.allele === "string") return item.allele;
  if (typeof item.length === "number") return `${item.length} aa`;
  return "Item";
}

function getBarValue(item: BarItem): number {
  if (typeof item.value === "number") return item.value;
  if (typeof item.strong_binders === "number") return item.strong_binders;
  if (typeof item.count === "number") return item.count;
  return 0;
}

export function ScientificViews({
  resultJson,
  views,
  exposeTestIds = true,
}: {
  resultJson: Record<string, unknown>;
  views: DeclarativeViewSpec[];
  exposeTestIds?: boolean;
}) {
  const effectiveViews: DeclarativeViewSpec[] =
    views.length > 0
      ? views
      : [
          {
            id: "summary-cards",
            kind: "metric_cards",
            title: "Run Summary Metrics",
            data_pointer: "/summary_cards",
          },
          {
            id: "length-dist",
            kind: "length_distribution",
            title: "Peptide Length Distribution",
            data_pointer: "/length_distribution",
          },
          {
            id: "allele-counts",
            kind: "bar",
            title: "Strong Binders Enrichment",
            data_pointer: "/allele_counts",
          },
          {
            id: "top-candidates",
            kind: "table",
            title: "Top Candidate Ligands",
            data_pointer: "/top_candidates",
          },
        ];

  return (
    <div
      className="sci-views-grid"
      data-testid={exposeTestIds ? "scientific-views" : undefined}
    >
      {effectiveViews.map((view) => {
        const raw = resolveJsonPointer(resultJson, view.data_pointer);
        if (!Array.isArray(raw) || raw.length === 0) {
          return null;
        }

        if (view.kind === "metric_cards") {
          const cards = raw as MetricCardItem[];
          return (
            <div
              key={view.id}
              className="sci-card sci-card-wide"
              data-testid={exposeTestIds ? `view-${view.id}` : undefined}
            >
              <div className="sci-card-title">{view.title}</div>
              <div className="metric-strip">
                {cards.map((c, idx) => (
                  <div key={`${c.label}-${idx}`} className="metric-tile">
                    <div className="metric-label">{c.label}</div>
                    <div className="metric-value">{String(c.value)}</div>
                  </div>
                ))}
              </div>
            </div>
          );
        }

        if (view.kind === "bar" || view.kind === "length_distribution") {
          const bars = raw as BarItem[];
          const maxVal = Math.max(1, ...bars.map(getBarValue));
          const svgWidth = 440;
          const rowHeight = 28;
          const svgHeight = bars.length * rowHeight + 16;

          return (
            <div
              key={view.id}
              className="sci-card"
              data-testid={exposeTestIds ? `view-${view.id}` : undefined}
            >
              <div className="sci-card-title">{view.title}</div>
              <svg
                viewBox={`0 0 ${svgWidth} ${svgHeight}`}
                className="sci-bar-svg"
                role="img"
                aria-label={view.title}
              >
                {bars.map((item, idx) => {
                  const label = getBarLabel(item);
                  const val = getBarValue(item);
                  const y = 8 + idx * rowHeight;
                  const barMaxW = 240;
                  const w = Math.max(3, (val / maxVal) * barMaxW);
                  const fill =
                    view.kind === "length_distribution"
                      ? "#3B6FD8"
                      : "var(--bc-accent, #E8612D)";
                  return (
                    <g key={`${label}-${idx}`} transform={`translate(0, ${y})`}>
                      <text
                        x={4}
                        y={15}
                        fill="var(--bc-text-muted)"
                        fontSize="11"
                        fontFamily="var(--bc-font-mono)"
                      >
                        {label}
                      </text>
                      <rect
                        x={135}
                        y={4}
                        width={barMaxW}
                        height={14}
                        rx={4}
                        fill="var(--bc-bg-subtle)"
                      />
                      <rect
                        x={135}
                        y={4}
                        width={w}
                        height={14}
                        rx={4}
                        fill={fill}
                      />
                      <text
                        x={135 + barMaxW + 10}
                        y={15}
                        fill="var(--bc-text)"
                        fontSize="11"
                        fontWeight="600"
                      >
                        {val}
                      </text>
                    </g>
                  );
                })}
              </svg>
            </div>
          );
        }

        if (view.kind === "table") {
          const rows = raw as Record<string, unknown>[];
          const firstRow = rows[0];
          if (!firstRow) return null;
          const cols = Object.keys(firstRow);
          return (
            <div
              key={view.id}
              className="sci-card sci-card-wide"
              data-testid={exposeTestIds ? `view-${view.id}` : undefined}
            >
              <div className="sci-card-title">{view.title}</div>
              <div className="table-scroll">
                <table className="data-table">
                  <thead>
                    <tr>
                      {cols.map((col) => (
                        <th key={col}>{col}</th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {rows.map((r, rIdx) => (
                      <tr key={rIdx}>
                        {cols.map((col) => (
                          <td
                            key={col}
                            className={col === "sequence" ? "mono accent-seq" : undefined}
                          >
                            {String(r[col] ?? "")}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </div>
          );
        }

        return null;
      })}
    </div>
  );
}
