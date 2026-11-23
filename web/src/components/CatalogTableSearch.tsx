import { useEffect, useRef, useState } from "react";
import { formatErrorInline, queryCatalog, toQueryAst } from "../api";
import type { CatalogQuery, CatalogQueryResult, FilterExpr, QueryTableResult } from "../types";

type Props = {
  preview: CatalogQueryResult;
  query: CatalogQuery;
  releaseVersion?: number;
  primaryKey: string[];
};
type Row = Record<string, unknown>;

export function CatalogTableSearch(props: Props) {
  const [open, setOpen] = useState(false);
  return <>
    <button type="button" className="bc-btn bc-btn-sm" onClick={() => setOpen(true)}>Search full table</button>
    {open && <SearchDialog {...props} onClose={() => setOpen(false)} />}
  </>;
}

function SearchDialog({ preview, query, releaseVersion, primaryKey, onClose }: Props & { onClose: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const request = useRef<AbortController | null>(null);
  const columns = preview.columns.map(item => item.name);
  const [column, setColumn] = useState(primaryKey.find(name => columns.includes(name)) ?? columns[0]!);
  const [term, setTerm] = useState("");
  const [result, setResult] = useState<QueryTableResult<Row> | null>(null);
  const [offsets, setOffsets] = useState([0]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const offset = offsets[offsets.length - 1]!;
  useEffect(() => {
    if (!dialog.current?.open) dialog.current?.showModal();
    return () => request.current?.abort();
  }, []);

  function reset() {
    request.current?.abort();
    setResult(null);
    setOffsets([0]);
    setError("");
    setBusy(false);
  }

  async function search(pages: number[]) {
    if (!term.length || !columns.includes(column)) return;
    request.current?.abort();
    const controller = new AbortController();
    request.current = controller;
    setError("");
    setBusy(true);
    setResult(null);
    const base = toQueryAst(query);
    const predicate: FilterExpr = { op: "starts_with", column, prefix: term };
    // When the table has no visible unique key, order by the whole projection.
    // Rows that still tie have identical visible values.
    const order = primaryKey.length && primaryKey.every(name => columns.includes(name)) ? primaryKey : columns;
    try {
      const next = await queryCatalog<Row>(preview.catalog_id, preview.grant_id, {
        ...base, table: preview.table, columns, snapshot_id: preview.snapshot_id,
        filter: base.filter ? { op: "and", exprs: [base.filter, predicate] } : predicate,
        order_by: order.map(name => ({ column: name, direction: "asc" })),
        limit: 25, offset: pages[pages.length - 1],
      }, releaseVersion, controller.signal);
      if (controller.signal.aborted) return;
      if (next.catalog_id !== preview.catalog_id || next.grant_id !== preview.grant_id ||
          next.table !== preview.table || next.snapshot_id !== preview.snapshot_id ||
          next.dataset_digest !== preview.dataset_digest || next.access_level !== "rows") {
        throw new Error("The provider returned a different catalog, snapshot or grant. Refresh the explorer.");
      }
      if (next.columns.length !== columns.length || next.columns.some((item, index) => item.name !== columns[index])) {
        throw new Error("The available columns changed. Refresh the explorer.");
      }
      setOffsets(pages);
      setResult(next);
    } catch (cause) {
      if (!controller.signal.aborted) setError(formatErrorInline(cause));
    } finally {
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  return <dialog ref={dialog} className="bc-modal bc-row-detail-modal" aria-label="Search full table"
    onClose={onClose} style={{ color: "var(--bc-text)", padding: 0 }}>
    <div className="bc-drawer-header">
      <div><h2 className="bc-drawer-title">Search full table</h2>
        <div className="bc-caption">{preview.table} · Snapshot {preview.snapshot_id}</div></div>
      <button type="button" className="bc-btn bc-btn-sm" onClick={() => dialog.current?.close()}>Close</button>
    </div>
    <div className="bc-drawer-body">
      <form style={{ display: "flex", flexWrap: "wrap", alignItems: "end", gap: 8 }}
        onSubmit={event => { event.preventDefault(); void search([0]); }}>
        <label style={{ display: "grid", gap: 4 }}>Column<select className="bc-input" value={column}
          onChange={event => { reset(); setColumn(event.target.value); }}>
          {columns.map(name => <option key={name}>{name}</option>)}
        </select></label>
        <label style={{ display: "grid", gap: 4 }}>Starts with (case-sensitive)<input className="bc-input" value={term} maxLength={512}
          onChange={event => { reset(); setTerm(event.target.value); }} /></label>
        <button className="bc-btn bc-btn-primary" disabled={!term.length || busy}>Search</button>
      </form>
      {busy && <p role="status">Searching…</p>}
      {error && <p role="alert">{error}</p>}
      {result && <>
        <p role="status">{result.rows.length
          ? result.total_in_scope === 1 ? "1 matching row" : `Rows ${offset + 1}–${offset + result.rows.length} of ${result.total_in_scope} matches`
          : result.total_in_scope === 0 ? "No matching rows." : "No rows returned in this page."}</p>
        {result.rows.length > 0 && <div className="table-scroll"><table className="data-table">
          <thead><tr>{result.columns.map(item => <th key={item.name}>{item.name}</th>)}</tr></thead>
          <tbody>{result.rows.map((row, index) => <tr key={offset + index}>
            {result.columns.map(({ name }) => <td key={name}>{typeof row[name] === "string"
              ? row[name] as string : JSON.stringify(row[name])}</td>)}
          </tr>)}</tbody>
        </table></div>}
        {result.remaining_daily_row_budget != null && <p className="bc-caption">Daily row allowance remaining: {result.remaining_daily_row_budget}</p>}
        {(offset > 0 || result.rows.length < result.total_in_scope) && <div className="bc-drawer-footer">
          <button type="button" className="bc-btn bc-btn-sm" disabled={offsets.length === 1}
            onClick={() => void search(offsets.slice(0, -1))}>Previous</button>
          <button type="button" className="bc-btn bc-btn-sm"
            disabled={!result.rows.length || offset + result.rows.length >= result.total_in_scope || result.remaining_daily_row_budget === 0}
            onClick={() => void search([...offsets, offset + result.rows.length])}>Next</button>
        </div>}
      </>}
    </div>
  </dialog>;
}
