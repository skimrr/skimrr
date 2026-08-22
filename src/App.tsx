import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import { Licence, Photo, Theme, TrashBatch, TrashResult, View, formatBytes } from "./types";
import { DuplicatesTab } from "./components/DuplicatesTab";
import { BlurTab } from "./components/BlurTab";
import { StatsTab } from "./components/StatsTab";
import { DaysTab, Day } from "./components/DaysTab";
import { TrashScreen } from "./components/TrashScreen";
import { ConfirmModal, EmptyTrashModal, UndoToast } from "./components/Overlays";
import { Lightbox } from "./components/Lightbox";
import { CompareView } from "./components/CompareView";
import { Guide } from "./components/Guide";
import { Settings } from "./components/Settings";
import { Activation } from "./components/Activation";

type Screen = "home" | "scanning" | "results" | "trash";

/* The fingerprint now only proposes: a model reviews every group it forms and drops the
   members that are different photographs. Measured on a real 138-photo trip folder,
   with and without that review:
       threshold 20 →  8 groups / 21 photos, reviewed:  5 /11
       threshold 28 → 20 groups / 50 photos, reviewed:  9 /21
       threshold 32 → 26 groups / 70 photos, reviewed: 10 /23
   Loosening no longer floods the list, so the slider can favour recall again: the
   default sits at 28 and it opens to 32, where the review still holds the result to
   ten groups. */
const DEFAULT_SIM_THRESHOLD = 28;

/* Laplacian variance is content-dependent. A fixed cutoff would flag every photo
   in a soft-textured library and none in a detailed one. Anchor it to this scan's
   own median instead: genuinely blurry frames sit far below it. */
/* Both ends are percentiles of the folder's own scores rather than multiples of the
   median, because the score has no absolute meaning: it measures how much fine detail
   a frame carries, and a folder of night streets sits an order of magnitude above a
   folder of misty landscapes.
   The ceiling matters as much as the default. On a real trip folder the scores ran
   from 0.73 to 4.69 while the slider stepped by whole numbers, so one notch went from
   flagging nothing to flagging 135 photographs out of 138. A tool that offers to bin
   most of a library has stopped being useful, so the slider cannot reach past the
   least sharp third. */
function blurRangeFor(scores: number[]): {
  threshold: number;
  max: number;
  median: number;
} {
  if (scores.length === 0) return { threshold: 0, max: 1, median: 0 };
  const sorted = [...scores].sort((a, b) => a - b);
  const at = (f: number) => sorted[Math.floor((sorted.length - 1) * f)];
  return {
    // Opens on the least sharp twentieth: enough to be worth a look, never a purge.
    threshold: at(0.05),
    max: Math.max(at(0.33), at(0.05) * 1.2, 0.001),
    median: at(0.5),
  };
}

function Wordmark({ className }: { className?: string }) {
  return (
    <span className={className}>
      skimr<em>r</em>
    </span>
  );
}

function DropIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.9"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M12 3v9" />
      <path d="M8.5 8.5 12 12l3.5-3.5" />
      <path d="M4.5 13.5v4a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2v-4" />
    </svg>
  );
}

function GearIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.9"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <path d="M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915" />
      <circle cx="12" cy="12" r="3" />
    </svg>
  );
}

export default function App() {
  const { t, i18n } = useTranslation();
  const [screen, setScreen] = useState<Screen>("home");
  const [progress, setProgress] = useState({ done: 0, total: 0, phase: 1 });
  const [view, setView] = useState<View | null>(null);
  const [kept, setKept] = useState<number[]>([]);
  const [tab, setTab] = useState<"days" | "duplicates" | "blur" | "stats">("days");
  const [simThreshold, setSimThreshold] = useState(DEFAULT_SIM_THRESHOLD);
  const [blurThreshold, setBlurThreshold] = useState(0);
  const [blurMax, setBlurMax] = useState(100);
  const [blurMedian, setBlurMedian] = useState(0);
  const [blurSelected, setBlurSelected] = useState<Set<number>>(new Set());
  const [pendingTrash, setPendingTrash] = useState<Photo[] | null>(null);
  const [batches, setBatches] = useState<TrashBatch[]>([]);
  const [confirmEmpty, setConfirmEmpty] = useState(false);
  /* Which photos the viewer is stepping through, and where it sits. `group` is set
     only when opened from a duplicate group, which is where "keep this one" applies. */
  const [viewer, setViewer] = useState<{
    indices: number[];
    at: number;
    group?: number;
  } | null>(null);
  const [toast, setToast] = useState<TrashResult | null>(null);
  const [lastFolder, setLastFolder] = useState<string | null>(
    () => localStorage.getItem("skimrr-last-folder"),
  );
  const [dropping, setDropping] = useState(false);
  /** Group being examined side by side, or null. */
  const [comparing, setComparing] = useState<number | null>(null);
  const [days, setDays] = useState<Day[]>([]);
  /** Days the work is narrowed to; empty means the whole folder. */
  const [pickedDays, setPickedDays] = useState<Set<string>>(new Set());
  /** Files the scan could not read because their contents live in the cloud. */
  const [offline, setOffline] = useState(0);
  const [offlineBytes, setOfflineBytes] = useState(0);
  const [downloading, setDownloading] = useState<{ done: number; total: number } | null>(null);
  const [stalled, setStalled] = useState<{ done: number; total: number } | null>(null);
  /* Where the keyboard is pointing in the duplicates list. Null until a key is used,
     so the mouse-only path is untouched and no focus ring appears unbidden. */
  const [cursor, setCursor] = useState<{ group: number; pos: number } | null>(null);
  const screenRef = useRef<Screen>("home");
  /** Bumped on cancel, so a scan that finishes late cannot take over the screen. */
  const scanToken = useRef(0);
  useEffect(() => {
    screenRef.current = screen;
  }, [screen]);
  const [error, setError] = useState<"scan" | "trash" | "library" | null>(null);
  const [theme, setTheme] = useState<Theme>(
    () => (localStorage.getItem("skimrr-theme") as Theme) ?? "auto",
  );
  const [settings, setSettings] = useState(false);
  const [guide, setGuide] = useState(
    () => localStorage.getItem("skimrr-guide-seen") !== "1",
  );
  const [licence, setLicence] = useState<Licence | null>(null);
  /* Held while the activation screen is up, so confirming resumes the exact batch the
     customer was about to move instead of making them start over. */
  const [askLicence, setAskLicence] = useState<Photo[] | null>(null);

  useEffect(() => {
    invoke<Licence>("licence_status")
      .then(setLicence)
      .catch(() => undefined);
  }, []);
  const toastTimer = useRef<number | undefined>(undefined);

  useEffect(() => {
    const root = document.documentElement;
    if (theme === "auto") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
    localStorage.setItem("skimrr-theme", theme);
  }, [theme]);

  useEffect(() => {
    const dl = listen<{ done: number; total: number }>("download-progress", (event) =>
      setDownloading({ done: event.payload.done, total: event.payload.total }),
    );
    const skipped = listen<number>("scan-skipped", (event) => {
      setOffline(event.payload);
      if (event.payload > 0) {
        invoke<{ count: number; bytes: number }>("offline_set")
          .then((set) => setOfflineBytes(set.bytes))
          .catch(() => undefined);
      }
    });
    return () => {
      skipped.then((fn) => fn()).catch(() => undefined);
      dl.then((fn) => fn()).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    const unlisten = listen<{ done: number; total: number; phase: number }>(
      "scan-progress",
      (event) => setProgress(event.payload),
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  const refresh = useCallback(async (threshold: number, only?: Set<string>) => {
    const picked = only ?? pickedDays;
    const next = await invoke<View>("regroup", {
      threshold,
      days: picked.size > 0 ? [...picked] : null,
    });
    setView(next);
    setKept(next.groups.map((g) => g.suggested));
    setBlurSelected(new Set());
    setViewer(null);
    return next;
  }, []);

  // Re-cluster (debounced) when the similarity slider moves.
  useEffect(() => {
    if (screen !== "results") return;
    const id = window.setTimeout(() => {
      refresh(simThreshold).catch(() => setError("scan"));
    }, 200);
    return () => window.clearTimeout(id);
  }, [simThreshold, screen, refresh]);

  async function chooseFolder() {
    setError(null);
    const folder = await open({ directory: true });
    if (!folder) return;
    await scanPath(folder);
  }

  /* Returning home is immediate: the running scan is told to stop, and its result is
     discarded when it eventually arrives. Waiting for the workers to notice would leave
     the button looking broken for as long as one image takes to decode. */
  /* Downloading is watched by polling stats rather than by reading the files, so the
     stop button works throughout and a stalled iCloud is reported, not endured. */
  async function downloadOffline() {
    setStalled(null);
    setDownloading({ done: 0, total: offline });
    try {
      await invoke<number>("download_offline");
      setDownloading(null);
      setOffline(0);
      await scanPath(lastFolder ?? "");
    } catch (e) {
      setDownloading(null);
      const m = /^stalled:(\d+)/.exec(String(e));
      if (m) setStalled({ done: Number(m[1]), total: offline });
    }
  }

  function toggleDay(key: string) {
    const next = new Set(pickedDays);
    next.has(key) ? next.delete(key) : next.add(key);
    setPickedDays(next);
    refresh(simThreshold, next).catch(() => setError("scan"));
  }

  function clearDays() {
    const empty = new Set<string>();
    setPickedDays(empty);
    refresh(simThreshold, empty).catch(() => setError("scan"));
  }

  function stopScan() {
    scanToken.current += 1;
    setScreen("home");
    invoke("cancel_scan").catch(() => undefined);
  }

  async function scanPath(folder: string) {
    setError(null);
    setLastFolder(folder);
    localStorage.setItem("skimrr-last-folder", folder);
    const token = ++scanToken.current;
    setOffline(0);
    setProgress({ done: 0, total: 0, phase: 1 });
    setScreen("scanning");
    setToast(null);
    try {
      await invoke<number>("scan_folder", { path: folder });
      if (token !== scanToken.current) return;
      setPickedDays(new Set());
      setDays(await invoke<Day[]>("days").catch(() => []));
      await loadTrash().catch(() => undefined);
      const next = await refresh(simThreshold);
      const scores = next.photos
        .map((p) => p.blur)
        .filter((b): b is number => b !== null);
      const range = blurRangeFor(scores);
      setBlurThreshold(range.threshold);
      setBlurMax(range.max);
      setBlurMedian(range.median);
      setTab("days");
      setScreen("results");
    } catch (e) {
      if (token !== scanToken.current) return;
      const reason = String(e);
      // A scan the user stopped is not a failure, so it must not look like one.
      if (reason.includes("library")) setError("library");
      else if (!reason.includes("cancelled")) setError("scan");
      setScreen("home");
    }
  }

  useEffect(() => {
    /* Dropping a folder on the window is the gesture people try first. Tauri reports
       paths rather than File objects, so a directory arrives like any other path and
       the backend rejects it if it is not one. */
    let pending: Promise<() => void> | null = null;
    try {
      pending = getCurrentWebview().onDragDropEvent((event) => {
        if (event.payload.type === "over") setDropping(true);
        else if (event.payload.type === "leave") setDropping(false);
        else if (event.payload.type === "drop") {
          setDropping(false);
          const first = event.payload.paths?.[0];
          if (first && screenRef.current !== "scanning") void scanPath(first);
        }
      });
    } catch {
      // Convenience, not a feature to die for: the app must still work without it.
      pending = null;
    }
    return () => {
      pending?.then((off) => off()).catch(() => undefined);
    };
    // Registered once; the live screen is read through screenRef.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /* Triage is the slow part of the job: a few hundred groups means a few hundred
     round trips to the mouse. These bindings collapse that to one hand. */
  useEffect(() => {
    if (screen !== "results" || tab !== "duplicates" || !view) return;

    function onKey(e: KeyboardEvent) {
      // Never steal a key from a text field, a modal, or the lightbox.
      const el = document.activeElement as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA")) return;
      if (viewer || pendingTrash || confirmEmpty || comparing !== null) return;
      const groups = view!.groups;
      if (groups.length === 0) return;

      const at = cursor ?? { group: 0, pos: 0 };
      const size = groups[at.group]?.indices.length ?? 0;
      let next: { group: number; pos: number } | null = null;

      switch (e.key) {
        case "ArrowRight":
          next = { group: at.group, pos: Math.min(at.pos + 1, size - 1) };
          break;
        case "ArrowLeft":
          next = { group: at.group, pos: Math.max(at.pos - 1, 0) };
          break;
        case "ArrowDown":
          next = { group: Math.min(at.group + 1, groups.length - 1), pos: 0 };
          break;
        case "ArrowUp":
          next = { group: Math.max(at.group - 1, 0), pos: 0 };
          break;
        case "Enter":
        case " ":
          setKept((k) => k.map((v, i) => (i === at.group ? at.pos : v)));
          setCursor(at);
          e.preventDefault();
          return;
        case "t":
        case "T":
          void trashGroup(at.group);
          setCursor(at);
          e.preventDefault();
          return;
        case "Escape":
          setCursor(null);
          return;
        default:
          return;
      }

      e.preventDefault();
      setCursor(next);
    }

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [screen, tab, view, cursor, viewer, pendingTrash, confirmEmpty, comparing]);

  // Keep the cursor on screen without yanking the page when the mouse is in charge.
  useEffect(() => {
    if (!cursor) return;
    document
      .querySelector('[data-cursor="true"]')
      ?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [cursor]);

  function showToast(result: TrashResult) {
    window.clearTimeout(toastTimer.current);
    setToast(result);
    toastTimer.current = window.setTimeout(() => setToast(null), 8000);
  }

  const loadTrash = useCallback(async () => {
    const list = await invoke<TrashBatch[]>("list_trash");
    setBatches(list);
    return list;
  }, []);

  async function confirmTrash(paths: string[]) {
    setPendingTrash(null);
    try {
      const result = await invoke<TrashResult>("trash_photos", { paths });
      await refresh(simThreshold);
      await loadTrash().catch(() => undefined);
      showToast(result);
    } catch {
      setError("trash");
      await refresh(simThreshold).catch(() => undefined);
    }
  }

  async function restoreBatch(batchId: string) {
    try {
      await invoke<number>("undo_trash", { batchId });
    } finally {
      await Promise.all([
        refresh(simThreshold).catch(() => undefined),
        loadTrash().catch(() => undefined),
      ]);
    }
  }

  async function emptyTrash() {
    setConfirmEmpty(false);
    try {
      await invoke<number>("empty_trash");
    } finally {
      setToast(null);
      await loadTrash().catch(() => undefined);
    }
  }

  async function undo() {
    if (!toast) return;
    const batchId = toast.batch_id;
    window.clearTimeout(toastTimer.current);
    setToast(null);
    try {
      await invoke<number>("undo_trash", { batchId });
    } finally {
      await refresh(simThreshold).catch(() => undefined);
    }
  }

  /** Reviewing is free; moving files is what the licence buys. */
  function requestTrash(photos: Photo[]) {
    if (photos.length === 0) return;
    if (licence?.activated) setPendingTrash(photos);
    else setAskLicence(photos);
  }

  function trashGroup(groupIndex: number) {
    if (!view) return;
    const group = view.groups[groupIndex];
    requestTrash(
      group.indices
        .filter((_, pos) => pos !== kept[groupIndex])
        .map((i) => view.photos[i]),
    );
  }

  function trashBlurSelection() {
    if (!view || blurSelected.size === 0) return;
    requestTrash([...blurSelected].map((i) => view.photos[i]));
  }

  const lang = i18n.language;
  const trashCount = batches.reduce((sum, b) => sum + b.photos.length, 0);
  const trashBytes = batches.reduce((sum, b) => sum + b.bytes, 0);
  const blurCount = view
    ? view.photos.filter((p) => p.blur !== null && p.blur < blurThreshold).length
    : 0;

  return (
    <div className={`app${dropping ? " dropping" : ""}`}>
      <header className="topbar">
        {screen !== "home" && <Wordmark className="brand" />}
        <span className="spacer" />
        {licence?.update_available && (
          <button
            className="update-link"
            onClick={() =>
              openUrl("https://skimrr.com/download").catch(() => undefined)
            }
            title={t("update.hint")}
          >
            {t("update.available", { version: licence.update_available })}
          </button>
        )}
        {(screen === "results" || screen === "trash") && trashCount > 0 && (
          <button
            className={`trash-link${screen === "trash" ? " on" : ""}`}
            onClick={() => {
              loadTrash().catch(() => undefined);
              setScreen(screen === "trash" ? "results" : "trash");
            }}
          >
            {t("trash.link")}
            <span className="count mono">{trashCount}</span>
          </button>
        )}
        {/* Langue, thème et guide vivaient chacun dans la barre. Réunis derrière un
            seul bouton, ils rendent le haut de l'écran au produit. */}
        <button
          className="theme-btn"
          onClick={() => setSettings(true)}
          aria-label={t("settings.title")}
          title={t("settings.title")}
        >
          <GearIcon />
        </button>
      </header>

      {screen === "home" && (
        <main className="home">
          <Wordmark className="wordmark brand" />
          <p className="tagline">{t("tagline")}</p>
          <div className={`dropzone${dropping ? " dropping" : ""}`}>
            <span className="dropzone-icon">
              <DropIcon />
            </span>
            <p className="dropzone-text">
              {dropping ? t("home.dropActive") : t("home.drop")}
            </p>
            <button className="btn-primary" onClick={chooseFolder}>
              {t("home.choose")}
            </button>
          </div>
          {lastFolder && (
            <button className="btn-quiet" onClick={() => scanPath(lastFolder)}>
              {t("scan.again", { name: lastFolder.split("/").pop() || lastFolder })}
            </button>
          )}
          <p className="privacy">{t("home.privacy")}</p>
          {error === "scan" && <p className="error">{t("error.scan")}</p>}
          {error === "library" && <p className="error">{t("error.library")}</p>}
        </main>
      )}

      {screen === "scanning" && (
        <main className="scanning">
          <span className="label">
            {progress.total === 0
              ? t("scan.preparing")
              : progress.phase === 1
                ? t("scan.hashing")
                : t("scan.sharpness")}
          </span>
          {progress.total > 0 && (
            <span className="counter mono">
              {progress.done.toLocaleString(lang)} /{" "}
              {progress.total.toLocaleString(lang)}
            </span>
          )}
          <span
            className={`progress${progress.total === 0 ? " indeterminate" : ""}`}
          >
            <i
              style={
                progress.total > 0
                  ? { width: `${(progress.done / progress.total) * 100}%` }
                  : undefined
              }
            />
          </span>
          <button className="btn-quiet" onClick={stopScan}>
            {t("scan.cancel")}
          </button>
        </main>
      )}

      {screen === "results" && view && (
        <main className="results">
          <div className="summary">
            <span className="text">
              {view.groups.length > 0
                ? t("results.summary", {
                    count: view.groups.length,
                    size: formatBytes(view.reclaimable_bytes, lang),
                  })
                : t("results.noneTitle")}
            </span>
            <span className="spacer" />
            <button className="btn-ghost" onClick={() => setScreen("home")}>
              {t("actions.back")}
            </button>
          </div>

          {offline > 0 && (
            <div className="notice">
              {downloading ? (
                <span>
                  {t("scan.downloading")}{" "}
                  <span className="mono">
                    {downloading.done} / {downloading.total}
                  </span>
                </span>
              ) : (
                <>
                  <span>
                    {t("scan.offlineAsk", {
                      count: offline,
                      size: formatBytes(offlineBytes, lang),
                    })}
                  </span>
                  <button className="btn-ghost" onClick={downloadOffline}>
                    {t("scan.download")}
                  </button>
                </>
              )}
            </div>
          )}
          {stalled && (
            <p className="notice">
              {t("scan.stalled", { done: stalled.done, total: stalled.total })}
            </p>
          )}

          <nav className="tabs">
            <button
              className={`tab${tab === "days" ? " on" : ""}`}
              onClick={() => setTab("days")}
            >
              {t("days.tab")}
              <span className="count mono">{days.length}</span>
            </button>
            <button
              className={`tab${tab === "duplicates" ? " on" : ""}`}
              onClick={() => setTab("duplicates")}
            >
              {t("tabs.duplicates")}
              <span className="count mono">{view.groups.length}</span>
            </button>
            <button
              className={`tab${tab === "blur" ? " on" : ""}`}
              onClick={() => setTab("blur")}
            >
              {t("tabs.blur")}
              <span className="count mono">{blurCount}</span>
            </button>

            <button
              className={`tab${tab === "stats" ? " on" : ""}`}
              onClick={() => setTab("stats")}
            >
              {t("stats.tab")}
            </button>
          </nav>

          {error === "trash" && <p className="error">{t("error.trash")}</p>}

          {pickedDays.size > 0 && tab !== "days" && (
            <p className="notice">
              {t("days.narrowed", { count: pickedDays.size })}
              <button className="btn-ghost" onClick={() => clearDays()}>
                {t("days.clear")}
              </button>
            </p>
          )}

          {tab === "days" && (
            <DaysTab
              days={days}
              selected={pickedDays}
              onToggle={toggleDay}
              onClear={clearDays}
            />
          )}

          {tab === "stats" && (
            <StatsTab view={view} blurThreshold={blurThreshold} />
          )}

          {tab === "duplicates" && (
            <DuplicatesTab
              view={view}
              kept={kept}
              onKeep={(gi, pos) =>
                setKept((k) => k.map((v, i) => (i === gi ? pos : v)))
              }
              simThreshold={simThreshold}
              onSimThreshold={setSimThreshold}
              onTrashGroup={trashGroup}
              onCompare={setComparing}
              cursor={cursor}
              onExpand={(gi, pos) =>
                setViewer({
                  indices: view.groups[gi].indices,
                  at: pos,
                  group: gi,
                })
              }
            />
          )}

          {tab === "blur" && (
            <BlurTab
              view={view}
              threshold={blurThreshold}
              max={blurMax}
              median={blurMedian}
              onThreshold={setBlurThreshold}
              selected={blurSelected}
              onToggle={(i) =>
                setBlurSelected((s) => {
                  const next = new Set(s);
                  if (next.has(i)) next.delete(i);
                  else next.add(i);
                  return next;
                })
              }
              onSelectAll={(indices) => setBlurSelected(new Set(indices))}
              onClear={() => setBlurSelected(new Set())}
              onTrashSelected={trashBlurSelection}
              onExpand={(indices, at) => setViewer({ indices, at })}
            />
          )}
        </main>
      )}

      {screen === "trash" && (
        <TrashScreen
          batches={batches}
          onRestore={restoreBatch}
          onEmpty={() => setConfirmEmpty(true)}
          onBack={() => setScreen(view ? "results" : "home")}
        />
      )}

      {askLicence && (
        <Activation
          onActivated={(next) => {
            setLicence(next);
            setPendingTrash(askLicence);
            setAskLicence(null);
          }}
          onClose={() => setAskLicence(null)}
        />
      )}

      {settings && (
        <Settings
          licence={licence}
          onLicence={setLicence}
          theme={theme}
          onTheme={setTheme}
          onGuide={() => {
            setSettings(false);
            setGuide(true);
          }}
          scanLoaded={screen === "results"}
          onCleared={() => {
            setSettings(false);
            setScreen("home");
          }}
          onClose={() => setSettings(false)}
        />
      )}

      {guide && (
        <Guide
          onClose={() => {
            localStorage.setItem("skimrr-guide-seen", "1");
            setGuide(false);
          }}
        />
      )}

      {comparing !== null && view && view.groups[comparing] && (
        <CompareView
          photos={view.photos}
          indices={view.groups[comparing].indices}
          kept={view.groups[comparing].indices[kept[comparing]]}
          onKeep={(photoIndex) => {
            const pos = view.groups[comparing!].indices.indexOf(photoIndex);
            if (pos >= 0) setKept((k) => k.map((v, i) => (i === comparing ? pos : v)));
          }}
          onClose={() => setComparing(null)}
        />
      )}

      {viewer && view && (
        <Lightbox
          photos={viewer.indices.map((i) => view.photos[i])}
          index={viewer.at}
          onIndex={(at) => setViewer({ ...viewer, at })}
          onClose={() => setViewer(null)}
          onKeep={
            viewer.group === undefined
              ? undefined
              : (pos) =>
                  setKept((k) =>
                    k.map((v, i) => (i === viewer.group ? pos : v)),
                  )
          }
          isKept={
            viewer.group !== undefined && kept[viewer.group] === viewer.at
          }
        />
      )}

      {pendingTrash && (
        <ConfirmModal
          photos={pendingTrash}
          onCancel={() => setPendingTrash(null)}
          onConfirm={confirmTrash}
        />
      )}
      {confirmEmpty && (
        <EmptyTrashModal
          count={trashCount}
          size={formatBytes(trashBytes, lang)}
          onCancel={() => setConfirmEmpty(false)}
          onConfirm={emptyTrash}
        />
      )}
      {toast && <UndoToast count={toast.count} onUndo={undo} />}
    </div>
  );
}
