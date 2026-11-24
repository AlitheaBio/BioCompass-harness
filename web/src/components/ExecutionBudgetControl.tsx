import { useState } from "react";
import type { ExecutionBudget } from "../api";

export function readExecutionBudget(form: HTMLFormElement): ExecutionBudget | undefined {
  const fields = new FormData(form);
  const mode = fields.get("execution-budget-mode");
  if (mode === "unlimited") return { mode };
  if (mode !== "finite") return undefined;
  const amount = String(fields.get("execution-budget-amount") ?? "");
  const match = /^(\d+)(?:\.(\d{1,3}))?$/.exec(amount);
  if (match) {
    const milliEur = Number(match[1] + (match[2] ?? "").padEnd(3, "0"));
    if (Number.isSafeInteger(milliEur) && milliEur > 0) {
      return { mode, limit_milli_eur: milliEur };
    }
  }
  throw new Error("Enter a positive EUR amount with up to three decimal places.");
}

export function ExecutionBudgetControl({ disabled }: { disabled: boolean }) {
  const [mode, setMode] = useState("ask");
  return (
    <>
      <label className="bc-composer-control" title="Total Org EUR budget the agent may spend on this message">
        Budget
        <select
          className="bc-composer-select"
          name="execution-budget-mode"
          data-testid="execution-budget-mode"
          aria-label="Budget for this execution"
          value={mode}
          disabled={disabled}
          onChange={(event) => setMode(event.target.value)}
        >
          <option value="ask">Ask first (€0.00)</option>
          <option value="finite">Set total…</option>
          <option value="unlimited">Unlimited</option>
        </select>
      </label>
      {mode === "finite" && (
        <label className="bc-composer-control">
          <input
            name="execution-budget-amount"
            data-testid="execution-budget-amount"
            aria-label="Total EUR for this execution"
            className="bc-composer-budget-amount mono"
            type="text"
            inputMode="decimal"
            pattern="[0-9]+(\.[0-9]{1,3})?"
            maxLength={17}
            title="Total for this execution, in EUR with up to three decimal places"
            disabled={disabled}
            required
            autoFocus
          />
          EUR
        </label>
      )}
    </>
  );
}
