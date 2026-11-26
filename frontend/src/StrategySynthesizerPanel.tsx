// Walk-Forward Edge Synthesizer UI for discovering, filtering (BH-FDR + Deflated Sharpe),
// previewing, testing, and saving v2 strategy scripts. Strictly paper-only.
import { useMemo, useState } from "react";
import { BookmarkPlus, CheckCircle2, Code2, FlaskConical, Play, Sparkles, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import {
  Badge,
  Button,
  StatusPill,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui";
import { ProvenanceTag } from "@/Charts";
import {
  type Dataset,
  type Home,
  type Market,
  type Slots,
  type Strategy,
  moneySigned,
} from "@/lib/desk";
import {
  type StrategyFamily,
  type SynthCandidate,
  type SynthExecutionMode,
  type SynthesizeResponse,
  saveStrategyScript,
  synthesizeStrategies,
} from "@/lib/research";

const FAMILY_LABELS: Record<StrategyFamily, string> = {
  all: "All families",
  momentum_microstructure: "Momentum microstructure",
  mean_reversion_pav: "Mean reversion PAV",
  maker_imbalance: "Maker imbalance",
};

function familyBadgeLabel(family: string): string {
  if (family === "momentum_microstructure") return "Momentum";
  if (family === "mean_reversion_pav") return "Mean Rev PAV";
  if (family === "maker_imbalance") return "Maker Imbalance";
  return family;
}

export function StrategySynthesizerPanel({
  home,
  dataset,
  markets,
  slots,
  strategies,
  onTestInSandbox,
  onLibraryMutated,
}: {
  home: Home | null;
  dataset: Dataset;
  markets: Market[];
  slots: Slots | null;
  strategies: Strategy[];
  onTestInSandbox: (source: string, name: string) => void;
  onLibraryMutated?: () => void | Promise<void>;
}) {
  const [family, setFamily] = useState<StrategyFamily>("all");
  const [executionMode, setExecutionMode] = useState<SynthExecutionMode>("maker");
  const [fdrAlphaInput, setFdrAlphaInput] = useState<string>("0.05");
  const [maxCandidatesInput, setMaxCandidatesInput] = useState<string>("12");
  const [running, setRunning] = useState(false);
  const [savingName, setSavingName] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string>("");
  const [result, setResult] = useState<SynthesizeResponse | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [survivorsOnly, setSurvivorsOnly] = useState(false);

  const selectedCandidate: SynthCandidate | null = useMemo(() => {
    if (!result || result.candidates.length === 0) return null;
    if (selectedId) {
      const hit = result.candidates.find((c) => c.id === selectedId);
      if (hit) return hit;
    }
    return result.candidates.find((c) => c.passedAllGates) ?? result.candidates[0];
  }, [result, selectedId]);

  const visibleCandidates = useMemo(() => {
    if (!result) return [];
    if (!survivorsOnly) return result.candidates;
    return result.candidates.filter((c) => c.passedAllGates);
  }, [result, survivorsOnly]);

  const handleSynthesize = async () => {
    const parsedAlpha = Number(fdrAlphaInput);
    const fdrAlpha = Number.isFinite(parsedAlpha) ? Math.min(0.5, Math.max(0.005, parsedAlpha)) : 0.05;
    const parsedMax = Number(maxCandidatesInput);
    const maxCandidates = Number.isFinite(parsedMax) ? Math.min(100, Math.max(1, Math.round(parsedMax))) : 12;

    setFdrAlphaInput(String(fdrAlpha));
    setMaxCandidatesInput(String(maxCandidates));
    setRunning(true);
    setErrorMsg("");

    try {
      const res = await synthesizeStrategies(
        {
          family,
          execution_mode: executionMode,
          fdr_alpha: fdrAlpha,
          max_candidates: maxCandidates,
          dataset,
        },
        home,
        { markets, slots, strategies },
      );
      setResult(res);
      const top = res.candidates.find((c) => c.passedAllGates) ?? res.candidates[0] ?? null;
      setSelectedId(top ? top.id : null);
      toast.success(
        `Evaluated ${res.candidatesEvaluated} candidates: ${res.survivorCount} survivor${res.survivorCount === 1 ? "" : "s"} passed BH-FDR and DSR.`,
      );
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setErrorMsg(msg);
      toast.error(`Synthesis failed: ${msg}`);
    } finally {
      setRunning(false);
    }
  };

  const handleSaveCandidate = async (cand: SynthCandidate) => {
    if (savingName) return;
    setSavingName(cand.name);
    setErrorMsg("");
    try {
      const saved = await saveStrategyScript(
        { name: cand.name, source: cand.source, overwrite: true },
        home,
      );
      toast.success(saved.message);
      await onLibraryMutated?.();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setErrorMsg(msg);
      toast.error(`Save failed: ${msg}`);
    } finally {
      setSavingName(null);
    }
  };

  return (
    <section
      id="strategy-synthesizer"
      data-strategy-synthesizer
      aria-label="Walk-Forward Edge Synthesizer"
      className="rounded-[10px] border border-hairline bg-surface p-4 shadow-(--elev-card)"
    >
      {/* Header */}
      <div className="flex flex-wrap items-start justify-between gap-3 border-b border-hairline pb-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <FlaskConical className="size-4 text-info-foreground" strokeWidth={1.5} aria-hidden />
            <h3 className="text-sm font-semibold text-ink">Walk-Forward Edge Synthesizer</h3>
            <ProvenanceTag kind="paper" />
          </div>
          <p className="mt-0.5 text-xs text-ink-2">
            Discover candidate v2 strategy scripts in sample and screen survivors with Benjamini-Hochberg FDR and Deflated Sharpe out of sample.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-2">
          {result ? (
            <StatusPill
              id="synth-status-pill"
              tone={result.survivorCount > 0 ? "success" : "warning"}
              className={
                result.survivorCount > 0 ? "text-success-foreground" : "text-warning-foreground"
              }
            >
              <span>
                {result.survivorCount} / {result.candidatesEvaluated} survivors
              </span>
            </StatusPill>
          ) : (
            <StatusPill id="synth-status-pill" tone="info" className="text-info-foreground">
              <span>Ready</span>
            </StatusPill>
          )}
        </div>
      </div>

      {/* Controls Bar */}
      <div className="mt-3 grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-5 lg:items-end">
        <div className="flex flex-col gap-1">
          <label htmlFor="synth-family" className="text-xs font-medium text-ink-2">
            Strategy Family
          </label>
          <select
            id="synth-family"
            value={family}
            disabled={running}
            onChange={(e) => setFamily(e.target.value as StrategyFamily)}
            className="min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          >
            {(Object.keys(FAMILY_LABELS) as StrategyFamily[]).map((key) => (
              <option key={key} value={key}>
                {FAMILY_LABELS[key]}
              </option>
            ))}
          </select>
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="synth-exec-mode" className="text-xs font-medium text-ink-2">
            Execution Mode
          </label>
          <select
            id="synth-exec-mode"
            value={executionMode}
            disabled={running}
            onChange={(e) => setExecutionMode(e.target.value as SynthExecutionMode)}
            className="min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          >
            <option value="maker">Maker (post-only)</option>
            <option value="taker">Taker (crossing)</option>
          </select>
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="synth-fdr-alpha" className="text-xs font-medium text-ink-2">
            BH-FDR Alpha
          </label>
          <input
            id="synth-fdr-alpha"
            type="number"
            min="0.005"
            max="0.5"
            step="0.01"
            value={fdrAlphaInput}
            disabled={running}
            onChange={(e) => setFdrAlphaInput(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          />
        </div>

        <div className="flex flex-col gap-1">
          <label htmlFor="synth-max-candidates" className="text-xs font-medium text-ink-2">
            Max Candidates
          </label>
          <input
            id="synth-max-candidates"
            type="number"
            min="1"
            max="100"
            step="1"
            value={maxCandidatesInput}
            disabled={running}
            onChange={(e) => setMaxCandidatesInput(e.target.value)}
            className="mono min-h-11 rounded-md border border-hairline bg-board px-2.5 text-xs text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
          />
        </div>

        <div className="flex flex-col justify-end">
          <Button
            id="synth-run-btn"
            variant="default"
            disabled={running}
            onClick={() => void handleSynthesize()}
            className="min-h-11 w-full gap-2 px-4 text-xs font-medium sm:min-h-9"
          >
            <Sparkles className="size-3.5" strokeWidth={1.5} aria-hidden />
            <span>{running ? "Synthesizing..." : "Synthesize Edges"}</span>
          </Button>
        </div>
      </div>

      {errorMsg && (
        <div
          id="synth-error"
          role="alert"
          className="mt-3 flex items-start gap-2 rounded-lg border border-dot-red/50 bg-board px-3 py-2 text-xs text-destructive-foreground"
        >
          <TriangleAlert className="mt-0.5 size-4 shrink-0" strokeWidth={1.5} aria-hidden />
          <span>{errorMsg}</span>
        </div>
      )}

      {!result ? (
        <div className="mt-3 rounded-lg border border-dashed border-hairline bg-board/40 px-4 py-3 text-xs text-ink-2">
          Run Synthesize Edges to generate causal v2 candidates, rank held-out OOS returns, and inspect surviving scripts before saving them to the strategy library.
        </div>
      ) : (
        <div className="mt-4 space-y-4">
          {/* Summary metrics strip */}
          <dl className="grid grid-cols-2 divide-y divide-hairline rounded-lg border border-hairline bg-board/40 sm:grid-cols-4 sm:divide-x sm:divide-y-0">
            <div className="p-2.5">
              <dt className="text-xs text-ink-3">IS / OOS Windows</dt>
              <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                {result.isWindows} / {result.oosWindows}
              </dd>
            </div>
            <div className="p-2.5">
              <dt className="text-xs text-ink-3">Candidates Tested</dt>
              <dd className="mono mt-0.5 text-sm font-semibold text-ink">
                {result.candidatesEvaluated}
              </dd>
            </div>
            <div className="p-2.5">
              <dt className="text-xs text-ink-3">BH-FDR + DSR Survivors</dt>
              <dd
                className={
                  "mono mt-0.5 text-sm font-semibold " +
                  (result.survivorCount > 0 ? "text-success-foreground" : "text-warning-foreground")
                }
              >
                {result.survivorCount}
              </dd>
            </div>
            <div className="p-2.5">
              <dt className="text-xs text-ink-3">Synthesizer Engine</dt>
              <dd className="mono mt-0.5 text-xs font-medium text-ink">
                {result.source === "server" ? "Server Synthesizer" : "Client Preview"} ({result.elapsedMs}ms)
              </dd>
            </div>
          </dl>

          <div className="flex flex-wrap items-center justify-between gap-2">
            <p className="text-xs text-ink-2">{result.honestyNote}</p>
            <Button
              id="synth-survivors-toggle"
              variant={survivorsOnly ? "default" : "outline"}
              size="sm"
              className="min-h-11 px-3 text-xs sm:min-h-8"
              onClick={() => setSurvivorsOnly((v) => !v)}
            >
              <span>{survivorsOnly ? "Show All Candidates" : "Survivors Only"}</span>
            </Button>
          </div>

          {/* Survivor & Candidate Table */}
          <div id="synth-table" className="overflow-hidden rounded-lg border border-hairline">
            {visibleCandidates.length === 0 ? (
              <p role="status" className="p-4 text-xs text-ink-2">
                No candidates cleared the BH-FDR and Deflated Sharpe thresholds under this configuration.
              </p>
            ) : (
              <Table className="stack">
                <TableHeader>
                  <TableRow>
                    <TableHead>Candidate</TableHead>
                    <TableHead>Side</TableHead>
                    <TableHead className="text-right">OOS Exp</TableHead>
                    <TableHead className="text-right">Sharpe</TableHead>
                    <TableHead className="text-right">Deflated SR</TableHead>
                    <TableHead className="text-right">BH Adj p</TableHead>
                    <TableHead className="text-right">Lib Corr</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead className="text-right">Actions</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {visibleCandidates.map((c) => {
                    const passed = c.passedAllGates;
                    const isSelected = selectedCandidate?.id === c.id;
                    return (
                      <TableRow
                        key={c.id}
                        data-synth-row={c.name}
                        aria-selected={isSelected ? "true" : undefined}
                        className={"cursor-pointer " + (isSelected ? "bg-blue-soft/60" : "hover:bg-board")}
                        onClick={() => setSelectedId(c.id)}
                      >
                        <TableCell className="m-grow max-w-[260px]">
                          <div className="flex flex-col gap-1">
                            <span className="mono break-all text-xs font-semibold text-ink">{c.name}</span>
                            <div className="flex flex-wrap items-center gap-1.5">
                              <Badge variant="secondary" className="text-xs">
                                {familyBadgeLabel(c.family)}
                              </Badge>
                              <Badge variant="outline" className="mono text-xs">
                                {c.executionMode.toUpperCase()}
                              </Badge>
                            </div>
                          </div>
                        </TableCell>
                        <TableCell>
                          <Badge variant="outline" className="mono text-xs">
                            {c.side.toUpperCase()}
                          </Badge>
                        </TableCell>
                        <TableCell
                          className={
                            "mono text-right text-xs font-semibold " +
                            (c.oosNetExpectancyCents > 0
                              ? "text-success-foreground"
                              : "text-destructive-foreground")
                          }
                        >
                          {c.oosNetExpectancyCents >= 0 ? "+" : ""}
                          {c.oosNetExpectancyCents.toFixed(2)}c
                          <div className="text-ink-3 font-normal">{moneySigned(c.oosNetPnlDollars)}</div>
                        </TableCell>
                        <TableCell className="mono text-right text-xs text-ink">
                          {c.oosSharpe !== null ? c.oosSharpe.toFixed(2) : "-"}
                        </TableCell>
                        <TableCell
                          className={
                            "mono text-right text-xs font-medium " +
                            (c.passedDsr ? "text-success-foreground" : "text-ink-2")
                          }
                        >
                          {c.deflatedSharpe.toFixed(2)}
                        </TableCell>
                        <TableCell
                          className={
                            "mono text-right text-xs font-medium " +
                            (c.survivedFdr ? "text-success-foreground" : "text-ink-2")
                          }
                        >
                          {c.bhAdjustedPValue.toFixed(4)}
                        </TableCell>
                        <TableCell className="mono text-right text-xs text-ink-2">
                          {c.maxCorrWithLibrary.toFixed(2)}
                        </TableCell>
                        <TableCell>
                          <StatusPill
                            tone={passed ? "success" : "warning"}
                            className={
                              "min-h-6 px-2 py-0.5 text-xs " +
                              (passed ? "text-success-foreground" : "text-warning-foreground")
                            }
                          >
                            {passed ? "Survivor" : "Filtered"}
                          </StatusPill>
                        </TableCell>
                        <TableCell className="text-right">
                          <div
                            className="flex flex-wrap justify-end gap-1.5"
                            onClick={(e) => e.stopPropagation()}
                          >
                            <Button
                              variant="outline"
                              size="sm"
                              className="min-h-11 px-2.5 text-xs sm:min-h-8"
                              onClick={() => setSelectedId(c.id)}
                            >
                              <span>Preview</span>
                            </Button>
                            <Button
                              variant="outline"
                              size="sm"
                              data-synth-test={c.name}
                              className="min-h-11 gap-1 px-2.5 text-xs sm:min-h-8"
                              onClick={() => onTestInSandbox(c.source, c.name)}
                            >
                              <Play className="size-3" aria-hidden />
                              <span>Test in Sandbox</span>
                            </Button>
                            <Button
                              variant="default"
                              size="sm"
                              data-synth-save={c.name}
                              disabled={savingName === c.name}
                              className="min-h-11 gap-1 px-2.5 text-xs sm:min-h-8"
                              onClick={() => void handleSaveCandidate(c)}
                            >
                              <BookmarkPlus className="size-3" aria-hidden />
                              <span>{savingName === c.name ? "Saving..." : "Save to Library"}</span>
                            </Button>
                          </div>
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            )}
          </div>

          {/* Generated Strategy Source Preview */}
          {selectedCandidate && (
            <div
              id="synth-preview"
              data-synth-preview
              className="rounded-lg border border-hairline bg-board/50 p-3.5"
            >
              <div className="flex flex-wrap items-center justify-between gap-2 border-b border-hairline pb-2.5">
                <div className="flex flex-wrap items-center gap-2">
                  <Code2 className="size-4 text-info-foreground" strokeWidth={1.5} aria-hidden />
                  <span className="mono text-xs font-semibold text-ink">
                    {selectedCandidate.name}.rn
                  </span>
                  <Badge variant="outline" className="mono text-xs">
                    {selectedCandidate.side.toUpperCase()}
                  </Badge>
                  {selectedCandidate.causalCanaryPassed && (
                    <StatusPill tone="success" className="min-h-6 px-2 py-0.5 text-xs text-success-foreground">
                      <CheckCircle2 className="size-3" aria-hidden />
                      <span>Causal 1-bar OK</span>
                    </StatusPill>
                  )}
                </div>

                <div className="flex flex-wrap items-center gap-2">
                  <Button
                    id="synth-test-sandbox-btn"
                    variant="outline"
                    size="sm"
                    className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-8"
                    onClick={() => onTestInSandbox(selectedCandidate.source, selectedCandidate.name)}
                  >
                    <Play className="size-3.5" aria-hidden />
                    <span>Test in Sandbox</span>
                  </Button>
                  <Button
                    id="synth-save-btn"
                    variant="default"
                    size="sm"
                    disabled={savingName === selectedCandidate.name}
                    className="min-h-11 gap-1.5 px-3 text-xs sm:min-h-8"
                    onClick={() => void handleSaveCandidate(selectedCandidate)}
                  >
                    <BookmarkPlus className="size-3.5" aria-hidden />
                    <span>
                      {savingName === selectedCandidate.name ? "Saving..." : "Save to Library"}
                    </span>
                  </Button>
                </div>
              </div>

              <p className="mt-2 text-xs text-ink-2">
                {selectedCandidate.description}
                {selectedCandidate.rejectionReason ? ` Filtered: ${selectedCandidate.rejectionReason}.` : ""}
              </p>

              <pre
                id="synth-source-preview"
                className="mono mt-2.5 max-h-64 overflow-auto rounded-md border border-hairline bg-surface p-3 text-xs leading-5 text-ink"
              >
                {selectedCandidate.source}
              </pre>
            </div>
          )}
        </div>
      )}
    </section>
  );
}
