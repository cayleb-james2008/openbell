// The update strip and the version line. The server checks GitHub in the background; this page asks what it knows.
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Expandable, ExpandableContent, ExpandableTrigger } from "@/components/ui/expandable";
import { getJson } from "@/lib/desk";

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
const LOCAL = ["localhost", "127.0.0.1", "[::1]"].includes(location.hostname);

const dismissedFor = (): string => { try { return localStorage.getItem(DISMISS_KEY) || ""; } catch { return ""; } };
const rememberDismiss = (v: string) => { try { localStorage.setItem(DISMISS_KEY, v); } catch { /* private window: the banner just returns */ } };

// Server text is mapped by substring; the raw text is kept only for the Details disclosure.
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
  if (!secs) return "";
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
  const statusRef = useRef<HTMLParagraphElement>(null);
  const showRef = useRef<HTMLButtonElement>(null);
  const restartRef = useRef<HTMLButtonElement>(null);

  const loadVersion = async () => {
    try {
      const v = (await getJson("/api/version")) as Version;
      if (typeof v.version === "string") setVersion(v);
    } catch { /* no version line when the server does not answer */ }
  };
  const loadUpdate = async () => {
    try {
      const s = (await getJson("/api/update")) as Snap;
      if (typeof s.mode === "string" && typeof s.available === "boolean") setSnap(s);
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
    try {
      const { ok, data } = await post("/api/update/check");
      if (!ok) throw new Error(data.error || "The check did not finish. Try again later.");
      setSnap(data);
      rememberDismiss("");
      setDismissed("");
      setRaw("");
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
    try {
      const { ok, data } = await post("/api/update/apply");
      if (!ok) throw new Error(data.error || "The update did not install. Try again later.");
      const msg = "Installed " + latest + ". Restart to use it.";
      setJustInstalled(msg);
      setSnap({ ...data, available: false, staged: data.staged || latest });
      toast.success(msg);
      setTimeout(() => restartRef.current?.focus(), 0);
    } catch (e) {
      const text = e instanceof Error ? e.message : String(e);
      setRaw(text);
      toast.error(plainUpdateError(text));
    }
    setBusy("");
  };

  const restart = async () => {
    setBusy("restart");
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
    setTimeout(() => showRef.current?.focus(), 0);
  };
  const showAgain = () => {
    rememberDismiss("");
    setDismissed("");
    setTimeout(() => statusRef.current?.focus(), 0);
  };

  const notes = (snap?.notes || "").trim();
  const staged = !!snap?.staged;
  const showBanner = !!snap && (staged || (snap.available && !!snap.latest && dismissed !== snap.latest));
  const hiddenByDismiss = !!snap && !staged && snap.available && !!snap.latest && dismissed === snap.latest;

  const resultLine = (): string => {
    if (!snap) return "";
    if (snap.mode === "off") return "Updates are off on this OpenBell.";
    if (snap.error) return plainUpdateError(snap.error);
    if (snap.staged) return snap.staged + " installed. Restart to use it.";
    if (snap.available && snap.latest) return "Version " + snap.latest + " is available.";
    if (!snap.checked_at) return "";
    return "You are on the newest version (checked " + checkedWords(snap.checked_at) + ").";
  };

  return {
    version, snap, notes, staged, showBanner, hiddenByDismiss, busy, raw, returnMsg, justInstalled,
    refs: { statusRef, showRef, restartRef },
    check, apply, restart, dismiss, showAgain, resultLine,
  };
}

type U = ReturnType<typeof useUpdates>;

export function UpdateStrip({ u }: { u: U }) {
  const { snap, notes } = u;
  const busy = u.busy !== "";
  if (!snap) return null;
  const notLocal = !LOCAL;
  const title = u.staged
    ? (u.justInstalled || (snap.staged + " installed. Restart to use it."))
    : "Version " + snap.latest + " is available";
  const applying = u.busy === "apply";
  const restarting = u.busy === "restart";
  return (
    <>
      <p className="sr-only" role="status" aria-live="polite">{u.busy === "apply" ? "Downloading and checking" : u.busy === "restart" ? "Restarting" : ""}</p>
      {u.showBanner && (
        <section aria-label="OpenBell update" className="flex flex-wrap items-center gap-x-4 gap-y-2 rounded-[10px] border border-hairline bg-surface px-4 py-2.5">
          <p ref={u.refs.statusRef} tabIndex={-1} className="min-w-0 flex-1 basis-64 text-sm text-ink outline-none">
            <span className="font-medium">{title}</span>
            {!u.staged && notes && <span className="mt-0.5 block whitespace-pre-line break-words text-sm text-ink-2">{notes}</span>}
            {!u.staged && (
              <span className="mt-0.5 block text-sm text-ink-2">Update now downloads it, checks it, and replaces this app. Restart to start using it.</span>
            )}
            {u.busy === "apply" && <span className="block text-sm text-ink-2">Downloading and checking...</span>}
            {restarting && <span className="block text-sm text-ink-2">Restarting...</span>}
            {u.returnMsg && <span className="block text-sm text-ink-2">{u.returnMsg}</span>}
          </p>
          <div className="flex flex-wrap items-center gap-2">
            {u.staged ? (
              <Button ref={u.refs.restartRef} variant="default" disabled={busy} onClick={u.restart}>
                {restarting ? "Restarting..." : "Restart now"}
              </Button>
            ) : notLocal ? (
              <span className="text-sm text-ink-2">To update, open OpenBell on the computer it runs on.</span>
            ) : snap.can_apply ? (
              <Button variant="default" disabled={busy} onClick={u.apply}>{applying ? "Downloading and checking..." : "Update now"}</Button>
            ) : (
              <span className="text-sm text-ink-2">{snap.dev_build ? "This is a developer copy, so it does not update itself. Install a released version to get updates." : "Update from the machine OpenBell runs on."}</span>
            )}
            {!u.staged && snap.release_url && (
              <Button variant="outline" render={<a href={snap.release_url} target="_blank" rel="noopener" />}>What&apos;s new</Button>
            )}
            {!u.staged && <Button variant="ghost" disabled={busy} onClick={u.dismiss}>Dismiss</Button>}
          </div>
          {u.raw && (
            <Expandable expandDirection="vertical" expandBehavior="push" className="w-full">
              <ExpandableTrigger className="text-sm text-ink-2 underline">Details</ExpandableTrigger>
              <ExpandableContent preset="fade"><pre className="mt-1 whitespace-pre-wrap break-words font-mono text-xs text-ink-2">{u.raw}</pre></ExpandableContent>
            </Expandable>
          )}
        </section>
      )}
    </>
  );
}

export function VersionRow({ u }: { u: U }) {
  const { version, snap } = u;
  if (!version) return null;
  const busy = u.busy !== "";
  const line = u.resultLine();
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-sm text-ink-2">
      {version.release_url
        ? <a href={version.release_url} target="_blank" rel="noopener" className="rounded-md text-ink underline-offset-4 hover:underline focus-visible:outline-2 focus-visible:outline-blue">OpenBell {version.version}</a>
        : <span className="text-ink">OpenBell {version.version}</span>}
      {u.hiddenByDismiss && <Button ref={u.refs.showRef} variant="outline" size="sm" onClick={u.showAgain}>Show update</Button>}
      {snap && LOCAL && snap.mode !== "off" && (
        <Button variant="outline" size="sm" disabled={busy} onClick={u.check}>{u.busy === "check" ? "Checking..." : "Check for updates"}</Button>
      )}
      {snap && !LOCAL && <span>To update, open OpenBell on the computer it runs on.</span>}
      {line && <p role="status" className="min-w-0 break-words">{line}</p>}
      {u.staged && u.justInstalled === "" && <p role="status">{snap?.staged} installed. Restart to use it.</p>}
    </div>
  );
}
