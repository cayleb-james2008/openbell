// Interactive Strategy Sandbox Editor, Persistent Library CRUD, and Paper Tape Backtester.
// Calls /api/strategy/validate, /api/backtest, POST /api/strategies/save, and DELETE /api/strategies/{name}.
// Strictly paper-only. Zero user-facing internal engine jargon (#155).
import { useEffect, useMemo, useRef, useState } from "react";
import {
  CheckCircle2,
  Code2,
  Copy,
  FilePlus2,
  Play,
  RotateCcw,
  Save,
  ShieldCheck,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import { toast } from "sonner";
import { Badge, Button, StatusPill } from "@/components/ui";
import { StrategyEquityChart, ProvenanceTag } from "@/Charts";
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  type Strategy,
  cleanStr,
  dayLabel,
  fetchStrategySource,
  isNum,
  moneySigned,
  timeUtcLabel,
} from "@/lib/desk";
import {
  type BacktestResult,
  type RuneValidationResult,
  DEFAULT_SANDBOX_RUNE,
  runRuneBacktest,
  validateRuneStrategy,
} from "@/lib/quant";
import {
  DEFAULT_V2_STRATEGY_SOURCE,
  V2_TICK_CONTEXT_FIELDS,
  deleteStrategyScript,
  extractStrategyNameFromSource,
  rewriteStrategyNameFn,
  saveStrategyScript,
  validateStrategySlug,
} from "@/lib/research";

export type SandboxSourcePayload = {
  name: string;
  source: string;
  seq: number;
};

export function RuneSandbox({
  strategies,
  markets,
  slots,
  home,
  dataset,
  initialStrategyName,
  initialSourcePayload,
  onLibraryMutated,
  onClose,
}: {
  strategies: Strategy[];
  markets: Market[];
  slots: Slots | null;
  home: Home | null;
  dataset: Dataset;
  initialStrategyName?: string | null;
  initialSourcePayload?: SandboxSourcePayload | null;
  onLibraryMutated?: () => void | Promise<void>;
  onClose?: () => void;
}) {
  const [source, setSource] = useState<string>(DEFAULT_SANDBOX_RUNE);
  const [loadedFrom, setLoadedFrom] = useState<string>("template");
  const [scriptName, setScriptName] = useState<string>("late-favourite-edge");
  const [loadingTemplate, setLoadingTemplate] = useState(false);
  const [validating, setValidating] = useState(false);
  const [backtesting, setBacktesting] = useState(false);
  const [mutatingLibrary, setMutatingLibrary] = useState(false);
  const [saveAsOpen, setSaveAsOpen] = useState(false);
  const [saveAsName, setSaveAsName] = useState("");
  const [deleteConfirmOpen, setDeleteConfirmOpen] = useState(false);
  const [crudError, setCrudError] = useState("");
  const [activeField, setActiveField] = useState<string>(V2_TICK_CONTEXT_FIELDS[0].field);
  const [validation, setValidation] = useState<RuneValidationResult | null>(null);
  const [result, setResult] = useState<BacktestResult | null>(null);

  const editorRef = useRef<HTMLTextAreaElement>(null);

  const strategyNames = useMemo(
    () =>
      strategies
        .map((s) => cleanStr(s.strategy, ""))
        .filter(Boolean)
        .sort((a, b) => a.localeCompare(b)),
    [strategies],
  );

  const loadLibraryStrategy = async (name: string) => {
    setSaveAsOpen(false);
    setDeleteConfirmOpen(false);
    setCrudError("");

    if (!name || name === "template") {
      setSource(DEFAULT_SANDBOX_RUNE);
      setLoadedFrom("template");
      setScriptName("late-favourite-edge");
      setValidation(null);
      setResult(null);
      return;
    }
    if (name === "template-v2") {
      setSource(DEFAULT_V2_STRATEGY_SOURCE);
      setLoadedFrom("template-v2");
      setScriptName("pav-maker-edge-v2");
      setValidation(null);
      setResult(null);
      return;
    }
    setLoadingTemplate(true);
    try {
      const res = await fetchStrategySource(name);
      setSource(res.source);
      setLoadedFrom(name);
      setScriptName(name);
      setValidation(null);
      setResult(null);
    } catch (e) {
      toast.error(`Could not load ${name}: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLoadingTemplate(false);
    }
  };

  useEffect(() => {
    if (initialStrategyName) {
      void loadLibraryStrategy(initialStrategyName);
    }
  }, [initialStrategyName]);

  useEffect(() => {
    if (!initialSourcePayload) return;
    setSource(initialSourcePayload.source);
    const cleanTarget = initialSourcePayload.name.trim() || "synthesized-strategy-v2";
    setScriptName(cleanTarget);
    setLoadedFrom(strategyNames.includes(cleanTarget) ? cleanTarget : "template-v2");
    setSaveAsOpen(false);
    setDeleteConfirmOpen(false);
    setCrudError("");
    setValidation(null);
    setResult(null);
  }, [initialSourcePayload]);

  const baselineStats = useMemo(() => {
    if (!slots) return null;
    if (loadedFrom && loadedFrom !== "template" && loadedFrom !== "template-v2") {
      const tot = slots.strategy_totals?.find((t) => t.strategy === loadedFrom);
      const row = strategies.find((s) => cleanStr(s.strategy, "") === loadedFrom);
      if (tot || row) {
        return {
          label: loadedFrom,
          pnl: tot && isNum(tot.paper_pnl_dollars) ? tot.paper_pnl_dollars : null,
          windows: row && isNum(row.windows_traded) ? row.windows_traded : tot?.windows_traded ?? null,
          meanCents: row && isNum(row.mean_cents_per_contract) ? row.mean_cents_per_contract : null,
        };
      }
    }
    const potDelta =
      isNum(slots.paper_equity_dollars) && isNum(slots.paper_pot_start)
        ? slots.paper_equity_dollars - slots.paper_pot_start
        : null;
    return {
      label: "Active Paper Pot",
      pnl: potDelta,
      windows: slots.trades ? slots.trades.length : null,
      meanCents: null,
    };
  }, [slots, strategies, loadedFrom]);

  const lineCount = useMemo(() => Math.max(1, source.split("\n").length), [source]);

  const activeFieldInfo = useMemo(
    () => V2_TICK_CONTEXT_FIELDS.find((f) => f.field === activeField) ?? V2_TICK_CONTEXT_FIELDS[0],
    [activeField],
  );

  const isLibraryBacked =
    (loadedFrom !== "template" && loadedFrom !== "template-v2") ||
    strategyNames.includes(scriptName.trim());

  const deleteTargetName =
    loadedFrom !== "template" && loadedFrom !== "template-v2" ? loadedFrom : scriptName.trim();

  const handleInsertField = (fieldName: string) => {
    setActiveField(fieldName);
    const token = `ctx.${fieldName}`;
    const el = editorRef.current;
    if (!el) {
      setSource((prev) => `${prev}${prev.endsWith("\n") ? "" : "\n"}${token}`);
      return;
    }
    const start = el.selectionStart ?? source.length;
    const end = el.selectionEnd ?? source.length;
    const next = source.slice(0, start) + token + source.slice(end);
    setSource(next);
    requestAnimationFrame(() => {
      el.focus();
      el.selectionStart = el.selectionEnd = start + token.length;
    });
  };

  const handleValidate = async () => {
    setValidating(true);
    try {
      const res = await validateRuneStrategy(source, home);
      setValidation(res);
      if (res.valid) {
        toast.success(`Strategy syntax valid (${res.name}, ${res.side.toUpperCase()}).`);
      } else {
        toast.error(`Validation found ${res.diagnostics.filter((d) => d.level === "error").length} issue(s).`);
      }
    } finally {
      setValidating(false);
    }
  };

  const handleBacktest = async () => {
    setBacktesting(true);
    try {
      const startEq = slots && isNum(slots.paper_pot_start) ? slots.paper_pot_start : 100;
      const res = await runRuneBacktest(source, dataset, home, markets, slots, startEq);
      setResult(res);
      setValidation({
        valid: res.valid,
        name: res.strategy,
        side: res.side,
        diagnostics: res.diagnostics,
        elapsedMs: res.elapsedMs,
        source: res.source,
      });
      if (res.valid) {
        toast.success(
          `Backtest complete: ${res.summary.windowsTraded} trades, ${moneySigned(res.summary.netPnlDollars)} paper P&L.`,
        );
      } else {
        toast.error("Backtest halted: fix strategy compile diagnostics first.");
      }
    } finally {
      setBacktesting(false);
    }
  };

  const persistScript = async (targetRaw: string, overwrite: boolean, nextSource?: string) => {
    const check = validateStrategySlug(targetRaw);
    if (!check.ok) {
      setCrudError(check.error);
      toast.error(check.error);
      return;
    }
    const slug = check.slug;
    const bodySource = rewriteStrategyNameFn(nextSource ?? source, slug);

    setMutatingLibrary(true);
    setCrudError("");
    try {
      const saved = await saveStrategyScript({ name: slug, source: bodySource, overwrite }, home);
      setSource(bodySource);
      setScriptName(saved.name);
      setLoadedFrom(saved.name);
      setSaveAsOpen(false);
      setDeleteConfirmOpen(false);
      toast.success(saved.message);
      await onLibraryMutated?.();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setCrudError(msg);
      toast.error(`Save failed: ${msg}`);
    } finally {
      setMutatingLibrary(false);
    }
  };

  const handleSave = async () => {
    const target =
      scriptName.trim() ||
      (loadedFrom !== "template" && loadedFrom !== "template-v2"
        ? loadedFrom
        : extractStrategyNameFromSource(source) || "custom-strategy-v2");
    await persistScript(target, true);
  };

  const handleClone = async () => {
    const baseRaw =
      (loadedFrom !== "template" && loadedFrom !== "template-v2" ? loadedFrom : scriptName.trim()) ||
      extractStrategyNameFromSource(source) ||
      "strategy";
    const baseClean = baseRaw.replace(/\.rn$/i, "").replace(/-copy(-\d+)?$/, "");
    let candidate = `${baseClean}-copy`;
    let suffix = 2;
    const existing = new Set(strategyNames);
    while (existing.has(candidate)) {
      candidate = `${baseClean}-copy-${suffix}`;
      suffix++;
    }
    await persistScript(candidate, false);
  };

  const handleConfirmDelete = async () => {
    if (!deleteTargetName) return;
    setMutatingLibrary(true);
    setCrudError("");
    try {
      const res = await deleteStrategyScript(deleteTargetName, home);
      setDeleteConfirmOpen(false);
      setLoadedFrom("template");
      setSource(DEFAULT_SANDBOX_RUNE);
      setScriptName("late-favourite-edge");
      setValidation(null);
      setResult(null);
      toast.success(res.message);
      await onLibraryMutated?.();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setCrudError(msg);
      toast.error(`Delete failed: ${msg}`);
    } finally {
      setMutatingLibrary(false);
    }
  };

  return (
    <div
      id="rune-sandbox"
      data-rune-sandbox
      className="rounded-[10px] border border-hairline bg-surface p-4 shadow-(--elev-card)"
    >
      {/* Header */}
      <div className="flex flex-wrap items-center justify-between gap-3 border-b border-hairline pb-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <Code2 className="size-4 text-info-foreground" strokeWidth={1.5} aria-hidden />
            <h3 className="text-sm font-semibold text-ink">Interactive Strategy Sandbox</h3>
            <ProvenanceTag kind="paper" />
          </div>
          <p className="mt-0.5 text-xs text-ink-2">
            Edit v1 or v2 strategy scripts, inspect compile checks, manage library files, and backtest on 15m tape windows.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          <label className="sr-only" htmlFor="sandbox-template-select">
            Load strategy template
          </label>
          <select
            id="sandbox-template-select"
            aria-label="Load strategy template"
            value={loadedFrom}
            disabled={loadingTemplate || mutatingLibrary}
            onChange={(e) => void loadLibraryStrategy(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          >
            <option value="template">v1 Starter template</option>
            <option value="template-v2">v2 TickContext template</option>
            {strategyNames.map((n) => (
              <option key={n} value={n}>
                {n}.rn
              </option>
            ))}
          </select>

          {onClose && (
            <Button variant="ghost" className="min-h-11 px-3 text-xs sm:min-h-9" onClick={onClose}>
              Hide sandbox
            </Button>
          )}
        </div>
      </div>

      {/* Persistent Library CRUD Toolbar */}
      <div
        id="sandbox-library-controls"
        className="mt-3 flex flex-wrap items-end justify-between gap-2.5 rounded-lg border border-hairline bg-board/50 p-3"
      >
        <div className="flex min-w-0 flex-1 flex-wrap items-end gap-2">
          <div className="flex min-w-[200px] flex-1 flex-col gap-1 sm:max-w-xs">
            <label htmlFor="sandbox-strategy-name" className="text-xs font-medium text-ink-2">
              Library Script Name
            </label>
            <input
              id="sandbox-strategy-name"
              type="text"
              value={scriptName}
              disabled={mutatingLibrary}
              placeholder="my-strategy-v2"
              onChange={(e) => setScriptName(e.target.value)}
              className="mono min-h-11 rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink placeholder:text-ink-3 outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>

          <Button
            id="sandbox-save-btn"
            variant="default"
            disabled={mutatingLibrary || validating || backtesting}
            onClick={() => void handleSave()}
            className="min-h-11 gap-1.5 px-3 text-xs font-medium sm:min-h-9"
          >
            <Save className="size-3.5" strokeWidth={1.5} aria-hidden />
            <span>{mutatingLibrary ? "Saving..." : "Save"}</span>
          </Button>

          <Button
            id="sandbox-save-as-btn"
            variant="outline"
            disabled={mutatingLibrary}
            onClick={() => {
              const base = (scriptName.trim() || "custom-strategy").replace(/\.rn$/i, "");
              setSaveAsName(`${base}-v2`);
              setSaveAsOpen((v) => !v);
              setDeleteConfirmOpen(false);
            }}
            className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-9"
          >
            <FilePlus2 className="size-3.5" strokeWidth={1.5} aria-hidden />
            <span>Save As</span>
          </Button>

          <Button
            id="sandbox-clone-btn"
            variant="outline"
            disabled={mutatingLibrary}
            onClick={() => void handleClone()}
            className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-9"
          >
            <Copy className="size-3.5" strokeWidth={1.5} aria-hidden />
            <span>Clone</span>
          </Button>

          <Button
            id="sandbox-delete-btn"
            variant="danger"
            disabled={mutatingLibrary || !isLibraryBacked}
            onClick={() => {
              setDeleteConfirmOpen((v) => !v);
              setSaveAsOpen(false);
            }}
            className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-9"
          >
            <Trash2 className="size-3.5" strokeWidth={1.5} aria-hidden />
            <span>Delete</span>
          </Button>
        </div>
      </div>

      {/* Inline Save As prompt */}
      {saveAsOpen && (
        <div
          id="sandbox-save-as-bar"
          className="mt-2 flex flex-wrap items-end gap-2 rounded-lg border border-hairline bg-board p-3"
        >
          <div className="flex min-w-[200px] flex-1 flex-col gap-1 sm:max-w-xs">
            <label htmlFor="sandbox-save-as-input" className="text-xs font-medium text-ink-2">
              New Strategy Script Name
            </label>
            <input
              id="sandbox-save-as-input"
              type="text"
              value={saveAsName}
              onChange={(e) => setSaveAsName(e.target.value)}
              placeholder="new-strategy-slug"
              className="mono min-h-11 rounded-md border border-hairline bg-surface px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
            />
          </div>
          <Button
            id="sandbox-save-as-confirm-btn"
            variant="default"
            disabled={mutatingLibrary}
            onClick={() => void persistScript(saveAsName, false)}
            className="min-h-11 px-3 text-xs sm:min-h-9"
          >
            Confirm Save As
          </Button>
          <Button
            variant="ghost"
            onClick={() => setSaveAsOpen(false)}
            className="min-h-11 px-3 text-xs sm:min-h-9"
          >
            Cancel
          </Button>
        </div>
      )}

      {/* Inline Delete confirmation */}
      {deleteConfirmOpen && (
        <div
          id="sandbox-delete-confirm-bar"
          role="alert"
          className="mt-2 flex flex-wrap items-center justify-between gap-2 rounded-lg border border-dot-red/50 bg-board px-3 py-2.5 text-xs"
        >
          <span className="text-ink">
            Delete <strong className="mono font-semibold">{deleteTargetName}.rn</strong> from the persistent library?
          </span>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              id="sandbox-delete-confirm-btn"
              variant="danger"
              disabled={mutatingLibrary}
              onClick={() => void handleConfirmDelete()}
              className="min-h-11 px-3 text-xs sm:min-h-8"
            >
              Confirm Delete
            </Button>
            <Button
              variant="ghost"
              onClick={() => setDeleteConfirmOpen(false)}
              className="min-h-11 px-3 text-xs sm:min-h-8"
            >
              Cancel
            </Button>
          </div>
        </div>
      )}

      {crudError && (
        <div
          id="sandbox-crud-error"
          role="alert"
          className="mt-2 flex items-start gap-2 rounded-lg border border-dot-red/50 bg-board px-3 py-2 text-xs text-destructive-foreground"
        >
          <TriangleAlert className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} aria-hidden />
          <span>{crudError}</span>
        </div>
      )}

      {/* V2 TickContext Causal Feature Reference Strip */}
      <div
        id="v2-tick-context-strip"
        data-v2-tick-context
        className="mt-3 rounded-lg border border-hairline bg-board/40 p-3"
      >
        <div className="flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2">
            <span className="text-xs font-semibold text-ink">
              V2 TickContext Causal Fields
            </span>
            <Badge variant="outline" className="mono text-xs">
              evaluate(ctx)
            </Badge>
            <span className="text-xs text-ink-3">
              Click any field to insert at cursor
            </span>
          </div>
          <Button
            id="sandbox-load-v2-btn"
            variant="outline"
            size="sm"
            onClick={() => void loadLibraryStrategy("template-v2")}
            className="min-h-11 px-2.5 text-xs sm:min-h-8"
          >
            Load v2 Template
          </Button>
        </div>

        <div className="mt-2 flex flex-wrap gap-1.5">
          {V2_TICK_CONTEXT_FIELDS.map((f) => {
            const active = activeField === f.field;
            return (
              <button
                key={f.field}
                type="button"
                data-tick-field={f.field}
                title={`${f.type} (${f.unit}): ${f.description}`}
                onMouseEnter={() => setActiveField(f.field)}
                onFocus={() => setActiveField(f.field)}
                onClick={() => handleInsertField(f.field)}
                className={
                  "mono min-h-11 cursor-pointer rounded border px-2 py-1 text-xs outline-none transition-[background-color,border-color,color] duration-(--dur-press) ease-(--ease-house) focus-visible:ring-2 focus-visible:ring-ring sm:min-h-7 " +
                  (active
                    ? "border-ink bg-surface font-medium text-ink"
                    : "border-hairline bg-board text-ink-2 hover:border-ink-3 hover:text-ink")
                }
              >
                {f.field}
              </button>
            );
          })}
        </div>

        <div id="v2-tick-field-detail" className="mt-2 flex flex-wrap items-center gap-2 text-xs text-ink-2">
          <span className="mono font-semibold text-ink">ctx.{activeFieldInfo.field}</span>
          <Badge variant="secondary" className="mono text-xs">
            {activeFieldInfo.type} ({activeFieldInfo.unit})
          </Badge>
          <span>{activeFieldInfo.description}</span>
        </div>
      </div>

      {/* Side-by-side Editor + Backtest Results */}
      <div className="mt-4 grid grid-cols-1 gap-4 lg:grid-cols-12">
        {/* Left: Code Editor & Controls */}
        <div className="flex flex-col gap-3 lg:col-span-6">
          <div className="overflow-hidden rounded-lg border border-hairline bg-board">
            <div className="flex items-center justify-between border-b border-hairline bg-surface px-3 py-1.5 font-mono text-xs text-ink-3">
              <span>
                {loadedFrom === "template" || loadedFrom === "template-v2"
                  ? `${scriptName || "sandbox"}.rn`
                  : `${loadedFrom}.rn`}
              </span>
              <span>{lineCount} lines, whole-cent sim</span>
            </div>
            <div className="flex">
              <div
                aria-hidden="true"
                className="mono select-none border-r border-hairline bg-surface/60 px-2.5 py-3 text-right text-xs leading-5 text-ink-3"
              >
                {Array.from({ length: lineCount }, (_, i) => (
                  <div key={i}>{i + 1}</div>
                ))}
              </div>
              <textarea
                ref={editorRef}
                id="rune-editor"
                aria-label="Strategy source code"
                spellCheck={false}
                value={source}
                onChange={(e) => setSource(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Tab") {
                    e.preventDefault();
                    const el = e.currentTarget;
                    const start = el.selectionStart;
                    const end = el.selectionEnd;
                    const next = source.slice(0, start) + "    " + source.slice(end);
                    setSource(next);
                    requestAnimationFrame(() => {
                      el.selectionStart = el.selectionEnd = start + 4;
                    });
                  }
                }}
                rows={12}
                className="mono min-h-[240px] w-full resize-y bg-transparent px-3 py-3 text-xs leading-5 text-ink outline-none"
              />
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <Button
              id="sandbox-backtest-btn"
              variant="default"
              className="min-h-11 gap-2 px-3.5 text-xs font-medium sm:min-h-9"
              disabled={backtesting || validating}
              onClick={() => void handleBacktest()}
            >
              <Play className="size-3.5" aria-hidden />
              <span>{backtesting ? "Backtesting..." : "Backtest on Tape"}</span>
            </Button>

            <Button
              id="sandbox-validate-btn"
              variant="outline"
              className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-9"
              disabled={validating || backtesting}
              onClick={() => void handleValidate()}
            >
              <ShieldCheck className="size-3.5" aria-hidden />
              <span>{validating ? "Validating..." : "Validate Strategy"}</span>
            </Button>

            <Button
              variant="ghost"
              className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-9"
              onClick={() => {
                setSource(DEFAULT_SANDBOX_RUNE);
                setLoadedFrom("template");
                setScriptName("late-favourite-edge");
                setSaveAsOpen(false);
                setDeleteConfirmOpen(false);
                setCrudError("");
                setValidation(null);
                setResult(null);
              }}
            >
              <RotateCcw className="size-3.5" aria-hidden />
              <span>Reset</span>
            </Button>
          </div>

          {/* Compile diagnostics */}
          {validation && (
            <div
              id="sandbox-diagnostics"
              role={validation.valid ? "status" : "alert"}
              className="rounded-lg border border-hairline bg-board p-3 text-xs"
            >
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="inline-flex items-center gap-1.5 font-semibold text-ink">
                  {validation.valid ? (
                    <CheckCircle2 className="size-4 text-success-foreground" aria-hidden />
                  ) : (
                    <TriangleAlert className="size-4 text-destructive-foreground" aria-hidden />
                  )}
                  <span>{validation.valid ? "Compile passed" : "Compile diagnostics"}</span>
                </span>
                <span className="mono text-ink-3">
                  {validation.source === "server" ? "Server Sandbox" : "Local evaluator"} ({validation.elapsedMs}ms)
                </span>
              </div>
              <ul className="mt-2 space-y-1">
                {validation.diagnostics.map((d, idx) => (
                  <li
                    key={idx}
                    className={
                      "mono " +
                      (d.level === "error"
                        ? "text-destructive-foreground"
                        : d.level === "warn"
                          ? "text-warning-foreground"
                          : "text-ink-2")
                    }
                  >
                    {d.line ? `L${d.line}: ` : ""}
                    {d.message}
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>

        {/* Right: Side-by-Side Backtest Results */}
        <div className="flex flex-col gap-3 lg:col-span-6" data-sandbox-results>
          {!result ? (
            <div className="flex flex-1 flex-col justify-between rounded-lg border border-dashed border-hairline bg-board/40 p-5">
              <div>
                <h4 className="text-sm font-semibold text-ink">Side-by-Side Paper Backtest Output</h4>
                <p className="mt-1 text-xs text-ink-2">
                  Press <strong className="font-medium text-ink">Backtest on Tape</strong> to score your strategy script against all settled 15m windows on the active tape.
                </p>
              </div>
              <dl className="mt-4 grid grid-cols-2 gap-2.5 text-xs">
                <div className="rounded-md border border-hairline bg-surface p-2.5">
                  <dt className="text-ink-3">Baseline Scope</dt>
                  <dd className="mono mt-0.5 font-medium text-ink">{baselineStats?.label || "Active Tape"}</dd>
                </div>
                <div className="rounded-md border border-hairline bg-surface p-2.5">
                  <dt className="text-ink-3">Baseline Paper P&amp;L</dt>
                  <dd className="mono mt-0.5 font-medium text-ink">
                    {baselineStats?.pnl !== null && baselineStats?.pnl !== undefined
                      ? moneySigned(baselineStats.pnl)
                      : "$0.00"}
                  </dd>
                </div>
              </dl>
            </div>
          ) : (
            <>
              {/* Metrics comparison cards */}
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex flex-wrap items-center gap-2">
                  <Badge variant="outline" className="mono text-xs">
                    {result.strategy}
                  </Badge>
                  <StatusPill tone={result.valid ? "success" : "error"} className="min-h-6 px-2 py-0.5 text-xs text-info-foreground">
                    {result.side.toUpperCase()}
                  </StatusPill>
                </div>
                <span className="mono text-xs text-ink-3">
                  {result.summary.windowsTraded} trades, {result.elapsedMs}ms
                </span>
              </div>

              <dl className="grid grid-cols-2 divide-y divide-hairline border-y border-hairline sm:grid-cols-3 sm:divide-x sm:divide-y-0">
                <div className="p-2.5">
                  <dt className="text-xs text-ink-3">Paper Net P&amp;L</dt>
                  <dd
                    className={
                      "mono mt-0.5 text-sm font-semibold " +
                      (result.summary.netPnlDollars >= 0 ? "text-success-foreground" : "text-destructive-foreground")
                    }
                  >
                    {moneySigned(result.summary.netPnlDollars)}
                  </dd>
                  {baselineStats?.pnl !== null && baselineStats?.pnl !== undefined && (
                    <span className="mono text-xs text-ink-3">
                      vs {moneySigned(baselineStats.pnl)}
                    </span>
                  )}
                </div>
                <div className="p-2.5">
                  <dt className="text-xs text-ink-3">Sharpe</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                    {result.summary.sharpe !== null ? result.summary.sharpe.toFixed(2) : "-"}
                  </dd>
                  <span className="mono text-xs text-ink-3">
                    Sortino {result.summary.sortino !== null ? result.summary.sortino.toFixed(2) : "-"}
                  </span>
                </div>
                <div className="p-2.5">
                  <dt className="text-xs text-ink-3">Max Drawdown</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-destructive-foreground">
                    -{result.summary.maxDrawdownPct.toFixed(2)}%
                  </dd>
                  <span className="mono text-xs text-ink-3">Fee drag {result.summary.feeDragPct.toFixed(1)}%</span>
                </div>
                <div className="p-2.5">
                  <dt className="text-xs text-ink-3">Brier Score</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                    {result.summary.brierScore !== null ? result.summary.brierScore.toFixed(4) : "-"}
                  </dd>
                </div>
                <div className="p-2.5">
                  <dt className="text-xs text-ink-3">Win Rate</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                    {result.summary.winRatePct.toFixed(1)}%
                  </dd>
                </div>
                <div className="p-2.5">
                  <dt className="text-xs text-ink-3">Mean / Contract</dt>
                  <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                    {result.summary.meanCentsPerContract >= 0 ? "+" : ""}
                    {result.summary.meanCentsPerContract.toFixed(2)}c
                  </dd>
                </div>
              </dl>

              {/* Equity curve */}
              <div className="pt-1">
                <StrategyEquityChart
                  points={result.equityCurve}
                  start={0}
                  height={146}
                  label={`Sandbox backtest paper curve for ${result.strategy}`}
                />
              </div>

              {/* Recent backtest paper trades */}
              <div>
                <div className="mb-1.5 flex items-center justify-between text-xs">
                  <span className="font-semibold text-ink">Backtest Trade List ({result.trades.length})</span>
                  <span className="mono text-ink-3">Latest 12 shown</span>
                </div>
                {result.trades.length === 0 ? (
                  <p className="py-2 text-xs text-ink-2">
                    Zero settled windows matched the active entry rules on this tape.
                  </p>
                ) : (
                  <ul className="max-h-44 divide-y divide-hairline overflow-y-auto border-t border-hairline">
                    {[...result.trades].reverse().slice(0, 12).map((t, idx) => (
                      <li key={idx} className="flex flex-wrap items-center justify-between gap-2 py-1.5 font-mono text-xs">
                        <span className="text-ink-2">
                          {dayLabel(t.closeEpoch)} {timeUtcLabel(t.closeEpoch)}
                        </span>
                        <span className="truncate text-ink">
                          {t.ticker} | {t.contracts}x @ {Math.round(t.price * 100)}c
                        </span>
                        <span className={t.pnlDollars >= 0 ? "font-medium text-success-foreground" : "font-medium text-destructive-foreground"}>
                          {moneySigned(t.pnlDollars)}
                        </span>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
