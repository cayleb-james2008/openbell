// The update strip and the version line. The server checks GitHub in the background; this page asks what it knows.
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { Button, Disclosure } from "@/components/ui";
import { getJson, isNum } from "@/lib/desk";

type Snap = {
  mode?: string;
  current?: string;
  latest?: string;
  available: boolean;
  staged?: string;
  can_apply?: boolean;
  dev_build?: boolean;
  notes?: string;
  release_url?: string;
  error?: string;
  checked_at?: number;
};
type Version = { version: string; release_url?: string };

const DISMISS_KEY = "openbell-update-dismissed";
const CHECK_MS = 30 * 60 * 1000;

export function isLoopbackHost(host = location.hostname): boolean {
  return ["localhost", "127.0.0.1", "[::1]", "::1"].includes(host.toLowerCase());
}

const dismissedFor = (): string => { try { return localStorage.getItem(DISMISS_KEY) || ""; } catch { return ""; } };
const rememberDismiss = (v: string) => { try { localStorage.setItem(DISMISS_KEY, v); } catch { /* private window: the banner just returns */ } };

// Server text is mapped by substring; the raw text is kept only for the Details disclosure, never URLs or OS strings in view.
export function plainUpdateError(raw: string): string {
  const t = raw || "";
  if (/could not reach|connection/i.test(t)) return "Could not reach GitHub. Check your internet connection, then try again.";
  if (/checksum/i.test(t)) return "The download did not pass its safety check, so it was not installed. Nothing changed. Try again in a few minutes.";
  if (/answered HTTP/i.test(t)) return "GitHub had a problem. Nothing changed. Try again in a few minutes.";
  if (/only allowed from this machine/i.test(t)) return "Updating works only on the computer OpenBell runs on. Open it there to update.";
  if (/does not run on this machine/i.test(t)) return "The new version does not run on this computer, so it was not installed. Nothing changed.";
  return "The update did not finish. Nothing changed. Try again in a few minutes.";
}

function checkedWords(secs?: number): string {
  if (!isNum(secs) || secs <= 0) return "";
  const mins = Math.round((Date.now() - secs * 1000) / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return mins + " min ago";
  return Math.round(mins / 60) + " h ago";
}

export function useUpdates() {
  const [version, setVersion] = useState<Version | null>(null);
  const [snap, setSnap] = useState<Snap | null>(null);
  const [dismissed, setDismissed] = useState(dismissedFor);
  const [justInstalled, setJustInstalled] = useState("");
  const [busy, setBusy] = useState<"" | "apply" | "restart" | "check">("");
  const [raw, setRaw] = useState("");
  const [returnMsg, setReturnMsg] = useState("");
  const [focusReq, setFocusReq] = useState<"" | "status" | "show" | "restart">("");
  const statusRef = useRef<HTMLParagraphElement>(null);
  const showRef = useRef<HTMLButtonElement>(null);
  const restartRef = useRef<HTMLButtonElement>(null);
  const local = isLoopbackHost();

  useEffect(() => {
    if (!focusReq) return;
    const target = focusReq === "status" ? statusRef.current : focusReq === "show" ? showRef.current : restartRef.current;
    if (target) {
      target.focus();
      setFocusReq("");
    }
  }, [focusReq, dismissed, snap, busy]);

  const loadVersion = async () => {
    try {
      const v = (await getJson("/api/version")) as Version;
      if (v && typeof v.version === "string") setVersion(v);
    } catch { /* no version line when the server does not answer */ }
  };
  const loadUpdate = async () => {
    try {
      const s = (await getJson("/api/update")) as Snap;
      if (s && typeof s.mode === "string" && typeof s.available === "boolean") setSnap(s);
    } catch { /* best effort: say nothing that cannot be verified */ }
  };
  useEffect(() => {
    loadVersion();
    loadUpdate();
    const id = setInterval(loadUpdate, CHECK_MS);
    return () => clearInterval(id);
  }, []);

  const post = async (path: string) => {
    const r = await fetch(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: "{}" });
    const data = (await r.json().catch(() => ({}))) as Snap & { error?: string };
    return { ok: r.ok, data };
  };

  const check = async () => {
    setBusy("check");
    setRaw("");
    try {
      const { ok, data } = await post("/api/update/check");
      if (!ok) throw new Error(data.error || "The check did not finish. Try again later.");
      setSnap(data);
      rememberDismiss("");
      setDismissed("");
      if (data.error) setRaw(data.error);
    } catch (e) {
      setRaw(e instanceof Error ? e.message : String(e));
    }
    setBusy("");
  };

  const apply = async () => {
    const latest = snap?.latest || "";
    setBusy("apply");
    setRaw("");
    statusRef.current?.focus();
    setFocusReq("status");
    try {
      const { ok, data } = await post("/api/update/apply");
      if (!ok) throw new Error(data.error || "The update did not install. Try again later.");
      const ver = data.staged || latest;
      const msg = ver + " installed. Restart to use it.";
      setJustInstalled(msg);
      setSnap({ ...data, available: false, staged: ver });
      toast.success(msg);
      setFocusReq("restart");
    } catch (e) {
      const text = e instanceof Error ? e.message : String(e);
      setRaw(text);
      toast.error(plainUpdateError(text));
    }
    setBusy("");
  };

  const restart = async () => {
    setBusy("restart");
    setRaw("");
    statusRef.current?.focus();
    try {
      const { ok, data } = await post("/api/update/restart");
      if (!ok) throw new Error(data.error || "OpenBell could not restart.");
    } catch (e) {
      // A dropped connection is the restart itself; any other failure is shown as it is.
      if (!(e instanceof TypeError)) {
        const text = e instanceof Error ? e.message : String(e);
        setRaw(text);
        toast.error(plainUpdateError(text));
        setBusy("");
        return;
      }
    }
    waitForReturn();
  };

  // Poll the version for up to a minute, then reload on the new process.
  const waitForReturn = () => {
    const started = Date.now();
    const tick = async () => {
      if (Date.now() - started > 60000) {
        setReturnMsg("OpenBell did not come back within a minute. Start it again: run openbell-rs, or restart its service. Then reload this page.");
        setBusy("");
        return;
      }
      try {
        const r = await fetch("/api/version", { cache: "no-store" });
        if (r.ok) { location.reload(); return; }
      } catch { /* still restarting */ }
      setTimeout(tick, 1000);
    };
    setTimeout(tick, 1000);
  };

  const dismiss = () => {
    if (!snap?.latest) return;
    rememberDismiss(snap.latest);
    setDismissed(snap.latest);
    setRaw("");
    setFocusReq("show");
  };
  const showAgain = () => {
    rememberDismiss("");
    setDismissed("");
    setFocusReq("status");
  };

  const notes = typeof snap?.notes === "string" ? snap.notes.trim() : "";
  const staged = typeof snap?.staged === "string" && !!snap.staged;
  const showBanner = !!snap && (staged || (snap.available && !!snap.latest && dismissed !== snap.latest));
  const hiddenByDismiss = !!snap && !staged && snap.available && !!snap.latest && dismissed === snap.latest;
  const checkErr = !showBanner ? (raw || snap?.error || "") : "";

  const resultLine = (): string => {
    if (checkErr) return plainUpdateError(checkErr);
    if (!snap) return "";
    if (snap.mode === "off") return "Updates are off on this OpenBell.";
    if (staged) return snap.staged + " installed. Restart to use it.";
    if (snap.available && snap.latest) return "Version " + snap.latest + " is available.";
    // After a fresh start (checked_at null), show the version only — never "Not checked yet".
    const ago = checkedWords(snap.checked_at);
    if (!ago) return "";
    return "You are on the newest version (checked " + ago + ").";
  };

  return {
    local, version, snap, notes, staged, showBanner, hiddenByDismiss, busy, raw, checkErr, returnMsg, justInstalled,
    refs: { statusRef, showRef, restartRef },
    check, apply, restart, dismiss, showAgain, resultLine,
  };
}

export type UpdatesHandle = ReturnType<typeof useUpdates>;
type U = UpdatesHandle;

function ErrorDetails({ raw }: { raw: string }) {
  return (
    <Disclosure label={<span className="underline">Details</span>}>
      <pre className="mt-1 whitespace-pre-wrap break-words rounded-md border border-hairline bg-board p-2.5 font-mono text-xs text-ink-2">{raw}</pre>
    </Disclosure>
  );
}

export function UpdateStrip({ u }: { u: U }) {
  const { snap, notes, local } = u;
  const busy = u.busy !== "";
  if (!snap) return null;
  const canApplyHere = local && !!snap.can_apply;
  const title = u.staged
    ? (u.justInstalled || (snap.staged + " installed. Restart to use it."))
    : "Version " + snap.latest + " is available";
  const applying = u.busy === "apply";
  const restarting = u.busy === "restart";
  const releaseUrl = typeof snap.release_url === "string" && /^https?:\/\//i.test(snap.release_url) ? snap.release_url : "";
  return (
    <>
      <p className="sr-only" role="status" aria-live="polite">
        {applying ? "Downloading and checking" : restarting ? "Restarting" : u.staged ? title : u.raw ? plainUpdateError(u.raw) : ""}
      </p>
      {u.showBanner && (
        <section id="update-banner" aria-label="OpenBell update" className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-[10px] border border-hairline bg-surface px-4 py-3">
          <p ref={u.refs.statusRef} tabIndex={-1} className="min-w-0 flex-1 basis-64 text-sm text-ink outline-none focus-visible:ring-2 focus-visible:ring-ring">
            <span className="font-medium">{title}</span>
            {!u.staged && notes && <span className="mt-0.5 block whitespace-pre-line break-words text-sm text-ink-2">{notes}</span>}
            {!u.staged && canApplyHere && (
              <span className="mt-0.5 block text-sm text-ink-2">Update now downloads it, checks it, and replaces this app. Restart to start using it.</span>
            )}
            {applying && <span className="mt-0.5 block text-sm text-ink-2">Downloading and checking…</span>}
            {restarting && <span className="mt-0.5 block text-sm text-ink-2">Restarting…</span>}
            {u.raw && <span role="alert" className="mt-1 block text-sm text-unread">{plainUpdateError(u.raw)}</span>}
            {u.returnMsg && <span className="mt-0.5 block text-sm text-ink-2">{u.returnMsg}</span>}
          </p>
          <div className="flex flex-wrap items-center gap-2">
            {!local ? (
              <span className="text-sm text-ink-2">To update, open OpenBell on the computer it runs on.</span>
            ) : u.staged ? (
              <Button ref={u.refs.restartRef} variant="default" className="min-h-11" disabled={busy} onClick={u.restart}>
                {restarting ? "Restarting…" : u.raw ? "Try again" : "Restart now"}
              </Button>
            ) : snap.can_apply ? (
              <Button variant="default" className="min-h-11" disabled={busy} onClick={u.apply}>
                {applying ? "Downloading and checking…" : u.raw ? "Try again" : "Update now"}
              </Button>
            ) : (
              <span className="text-sm text-ink-2">
                {snap.dev_build
                  ? "This is a developer copy, so it does not update itself. Install a released version to get updates."
                  : "To update, open OpenBell on the computer it runs on."}
              </span>
            )}
            {!u.staged && releaseUrl && (
              <a
                href={releaseUrl}
                target="_blank"
                rel="noopener"
                className="inline-flex min-h-11 items-center justify-center rounded-md border border-hairline bg-surface px-3.5 text-sm font-medium text-ink hover:bg-board focus-visible:ring-2 focus-visible:ring-ring sm:min-h-9"
              >
                What&apos;s new
              </a>
            )}
            {!u.staged && <Button variant="ghost" className="min-h-11" disabled={busy} onClick={u.dismiss}>Dismiss</Button>}
          </div>
          {u.raw && <ErrorDetails raw={u.raw} />}
        </section>
      )}
      {u.hiddenByDismiss && !u.showBanner && (
        <div className="flex items-center justify-between gap-3 rounded-[10px] border border-hairline bg-surface px-4 py-2 text-xs text-ink-2">
          <span>Update {snap.latest} dismissed.</span>
          <Button ref={u.refs.showRef} variant="outline" className="min-h-11 text-xs" onClick={u.showAgain}>
            Show update
          </Button>
        </div>
      )}
    </>
  );
}

export function VersionRow({ u }: { u: U }) {
  const { version, snap, local } = u;
  if (!version) return null;
  const busy = u.busy !== "";
  const line = u.resultLine();
  const relUrl = typeof version.release_url === "string" && /^https?:\/\//i.test(version.release_url) ? version.release_url : "";
  return (
    <div id="version-row" className="flex flex-wrap items-center gap-x-4 gap-y-2 text-sm text-ink-2">
      {relUrl
        ? <a id="version-link" href={relUrl} target="_blank" rel="noopener" className="inline-flex min-h-11 items-center rounded-md text-ink underline-offset-4 outline-none hover:underline active:bg-track focus-visible:ring-2 focus-visible:ring-ring">OpenBell {version.version}</a>
        : <span className="text-ink">OpenBell {version.version}</span>}
      {u.hiddenByDismiss && <Button variant="outline" className="min-h-11" onClick={u.showAgain}>Show update</Button>}
      {local && (!snap || snap.mode !== "off") && (
        <Button id="check-updates" variant="outline" className="min-h-11" disabled={busy} onClick={u.check}>
          {u.busy === "check" ? "Checking..." : u.checkErr ? "Try again" : "Check for updates"}
        </Button>
      )}
      {!local && !u.showBanner && <span>To update, open OpenBell on the computer it runs on.</span>}
      {line && <p id="check-result" role="status" className={"min-w-0 break-words " + (u.checkErr ? "text-unread" : "")}>{line}</p>}
      {u.checkErr && <ErrorDetails raw={u.checkErr} />}
    </div>
  );
}
