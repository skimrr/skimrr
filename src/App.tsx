import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import { Licence, Photo, Theme, TrashBatch, TrashResult, View, formatBytes } from "./types";
import { DuplicatesTab } from "./components/DuplicatesTab";
import { BadShotTab, isBadShot } from "./components/BadShotTab";
import { StatsTab, keptPaths } from "./components/StatsTab";
import { DaysTab, Day } from "./components/DaysTab";
import { TrashScreen } from "./components/TrashScreen";
import { ConfirmModal, DayGridModal, EmptyTrashModal, UndoToast } from "./components/Overlays";
import { Lightbox } from "./components/Lightbox";
import { CompareView } from "./components/CompareView";
import { Guide } from "./components/Guide";
import { Settings } from "./components/Settings";
import { Activation } from "./components/Activation";
import {
  ExportModal,
  OpenProjectModal,
  SendModal,
  chooseProjectFile,
} from "./components/ProjectFile";

type Screen = "home" | "scanning" | "results" | "trash";

/* What the "share these to an editor" picker is allowed to select. A macOS app is a
   bundle — a directory — and an open panel only offers it when its extension is
   declared, so the filter is what makes Lightroom.app selectable at all. On Windows
   and Linux the editor is a plain executable whose name carries no reliable extension,
   so any filter there would hide the very thing being looked for. */
const APP_FILTERS = navigator.userAgent.includes("Mac")
  ? [{ name: "Application", extensions: ["app"] }]
  : undefined;

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
   Bad Shot replaced the slider this once fed with its own filters, and the Rust side
   now draws the same percentile for its verdicts; what survives here is the threshold
   the Overview still reads to split blurry from the rest of its donut. */
function blurRangeFor(scores: number[]): { threshold: number } {
  if (scores.length === 0) return { threshold: 0 };
  const sorted = [...scores].sort((a, b) => a - b);
  const at = (f: number) => sorted[Math.floor((sorted.length - 1) * f)];
  // Opens on the least sharp twentieth: enough to be worth a look, never a purge.
  return { threshold: at(0.05) };
}

/** Mirrors the Rust side's own `day_key`: local calendar date, zero-padded, so a
    photo's `taken` timestamp groups under exactly the same key its Day card uses. */
function dayKeyOf(unixSeconds: number): string {
  const d = new Date(unixSeconds * 1000);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** Same formatting DaysTab uses for a day's own label, for the grid modal's title. */
function formatDayLabel(key: string, lang: string): string {
  return new Intl.DateTimeFormat(lang, { dateStyle: "full" }).format(new Date(`${key}T12:00:00`));
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

function CloseIcon() {
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
      <path d="M6 6l12 12M18 6 6 18" />
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
  const [tab, setTab] = useState<"days" | "duplicates" | "badshot" | "addFolders" | "stats">("days");
  const [simThreshold, setSimThreshold] = useState(DEFAULT_SIM_THRESHOLD);
  const [blurThreshold, setBlurThreshold] = useState(0);
  const [blurSelected, setBlurSelected] = useState<Set<number>>(new Set());
  const [pendingTrash, setPendingTrash] = useState<Photo[] | null>(null);
  const [batches, setBatches] = useState<TrashBatch[]>([]);
  const [confirmEmpty, setConfirmEmpty] = useState(false);
  /* Resolved once at startup: which Photos library the Gallery tab's "include Photos"
     choice checks against. */
  const [libraryPath, setLibraryPath] = useState<string | null>(null);
  /* Which photos the viewer is stepping through, and where it sits. `group` is set
     only when opened from a duplicate group, which is where "keep this one" applies. */
  const [viewer, setViewer] = useState<{
    indices: number[];
    at: number;
    group?: number;
  } | null>(null);
  /* A day's photos, opened from the Gallery tab as a grid first — the contact-sheet
     view lets you see the whole day before committing to one photo full-screen.
     `dayGrid` holds that grid; `dayViewer` (opened from a click inside it, stacked on
     top rather than replacing it) is the full-screen step. Neither has a "keep this
     one" — there is no duplicate group here — and the photos come straight from the
     caller rather than indices into `view.photos`, since a Photos-library day has no
     place in that array at all. */
  const [dayGrid, setDayGrid] = useState<{ title: string; photos: Photo[] } | null>(null);
  const [dayViewer, setDayViewer] = useState<{ photos: Photo[]; at: number } | null>(null);
  /* Two or more photos picked in the day grid, for a side-by-side look — the same
     CompareView the Duplicates tab uses, but with no "keep" action of its own. */
  const [dayCompare, setDayCompare] = useState<Photo[] | null>(null);
  const [toast, setToast] = useState<TrashResult | null>(null);
  const [lastFolders, setLastFolders] = useState<string[] | null>(() => {
    try {
      const raw = localStorage.getItem("skimrr-last-folders");
      return raw ? (JSON.parse(raw) as string[]) : null;
    } catch {
      return null;
    }
  });
  const [dropping, setDropping] = useState(false);
  /** Group being examined side by side, or null. */
  const [comparing, setComparing] = useState<number | null>(null);
  const [days, setDays] = useState<Day[]>([]);
  /** Same shape, but from the Destination library, scoped to exactly the Source
      scan's own days — shown alongside `days` in the Gallery tab so a day can be
      recognised as "already safe" without opening Import. An explicit choice
      (`includePhotosInGallery`), not automatic: nobody asking "what do I already have
      from this trip" wants the whole library's history pulled in unasked. */
  const [photosDays, setPhotosDays] = useState<Day[]>([]);
  const [photosDaysLoading, setPhotosDaysLoading] = useState(false);
  /* Which half of "include Photos" is running. Reading the library is usually quick;
     merging the assets into the scan and re-clustering them is not, and the two are
     worth telling apart while the user waits. */
  const [photosPhase, setPhotosPhase] = useState<"reading" | "merging" | null>(null);
  /** Distinct from `photosDays.length > 0`: a real check that found nothing must not
      look the same as "never checked". */
  const [photosDaysChecked, setPhotosDaysChecked] = useState(false);
  /** A failed check (most commonly: no Full Disk Access yet) used to look identical to
      "checked, found nothing" — silent, with no way to tell the two apart from the
      button alone. This makes the failure itself visible. */
  const [photosDaysError, setPhotosDaysError] = useState(false);
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
  const [error, setError] = useState<
    "scan" | "trash" | "library" | "share" | null
  >(null);
  /* The editor photos get handed to, remembered so every use after the first is a
     single click. Forgotten again on failure, so a stale path — the app moved,
     renamed or uninstalled — cannot leave the button permanently broken. */
  const [shareApp, setShareApp] = useState<string | null>(() =>
    localStorage.getItem("skimrr-share-app"),
  );
  /** Open while the export dialog is up. */
  const [exporting, setExporting] = useState(false);
  /** Open while the "what leaves Skimrr" dialog is up. */
  const [sending, setSending] = useState(false);
  /* The `.skimrr` waiting to be opened — chosen from the picker, or handed over by the
     operating system when one was double-clicked. Held rather than opened straight
     away: an encrypted project needs a password, and any project replaces what is on
     screen, so neither should happen without being asked. */
  const [openingProject, setOpeningProject] = useState<string | null>(null);

  const shareTo = useCallback(
    async (paths: string[]) => {
      setError(null);
      let target = shareApp;
      if (!target) {
        const picked = await open({
          multiple: false,
          directory: false,
          title: t("stats.sharePick"),
          filters: APP_FILTERS,
        });
        if (typeof picked !== "string") return;
        target = picked;
      }
      try {
        await invoke("share_to_app", { appPath: target, paths });
        localStorage.setItem("skimrr-share-app", target);
        setShareApp(target);
      } catch {
        localStorage.removeItem("skimrr-share-app");
        setShareApp(null);
        setError("share");
      }
    },
    [shareApp, t],
  );
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
    invoke<string | null>("default_photos_library_path")
      .then(setLibraryPath)
      .catch(() => undefined);
  }, []);

  /* Decoupled from the scan flow on purpose: the default library path resolves
     asynchronously (a moment after mount), so a scan started before it lands would
     otherwise never pick up the Gallery's Photos-side days. Refetching whenever
     either changes covers that race, and also covers picking a different library
     from the Import tab after a scan is already showing. Failures (no Full Disk
     Access, no library) just leave the Photos side of the Gallery empty. */
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

  /* `refresh` must keep a stable identity — the slider effect lists it as a dependency,
     and a new function on every render would re-fire the effect. But a callback with no
     dependencies also freezes whatever state it closes over, which is how the day
     narrowing used to be lost: seven of the ten call sites pass no `only`, so they were
     all reading the empty Set from the first render. Selecting days and then moving the
     slider re-clustered the whole library while the "narrowed to N days" chip stayed on
     screen. A ref keeps the identity stable and the reading current. */
  const pickedDaysRef = useRef(pickedDays);
  useEffect(() => {
    pickedDaysRef.current = pickedDays;
  }, [pickedDays]);
  const viewRef = useRef<View | null>(null);
  useEffect(() => {
    viewRef.current = view;
  }, [view]);

  const refresh = useCallback(
    async (threshold: number, only?: Set<string>, keepPhotos = false) => {
      const picked = only ?? pickedDaysRef.current;
      const days = picked.size > 0 ? [...picked] : null;
      /* Moving the similarity slider cannot change a single photograph, so the backend
         is told not to send them: 21.4 MB of the 21.8 MB payload, on every 200 ms tick,
         to say nothing new. Only worth asking for when we still hold a set to keep. */
      const have = viewRef.current?.photos;
      const wantPhotos = !keepPhotos || !have || have.length === 0;

      let next = await invoke<View>("regroup", { threshold, days, withPhotos: wantPhotos });
      if (!wantPhotos) {
        if (have && have.length === next.total_files) {
          next = { ...next, photos: have };
        } else {
          /* The set of photographs moved under us between the two calls, so the indices
             in `next.groups` no longer point at the ones we kept. Ask again properly
             rather than render a group pointing at the wrong picture. */
          next = await invoke<View>("regroup", { threshold, days, withPhotos: true });
        }
      }
      setView(next);
      setKept(next.groups.map((g) => g.suggested));
      setBlurSelected(new Set());
      setViewer(null);
      return next;
    },
    [],
  );

  // Re-cluster (debounced) when the similarity slider moves.
  useEffect(() => {
    if (screen !== "results") return;
    const id = window.setTimeout(() => {
      // The one call that can keep the photographs it already has.
      refresh(simThreshold, undefined, true).catch(() => setError("scan"));
    }, 200);
    return () => window.clearTimeout(id);
  }, [simThreshold, screen, refresh]);

  async function chooseFolder() {
    setError(null);
    const folders = await open({ directory: true, multiple: true });
    if (!folders || folders.length === 0) return;
    await scanFolders(folders);
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
      await scanFolders(lastFolders ?? []);
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

  async function scanFolders(folders: string[]) {
    if (folders.length === 0) return;
    setError(null);
    setLastFolders(folders);
    localStorage.setItem("skimrr-last-folders", JSON.stringify(folders));
    const token = ++scanToken.current;
    setPhotosDays([]);
    setPhotosDaysChecked(false);
    setOffline(0);
    setProgress({ done: 0, total: 0, phase: 1 });
    setScreen("scanning");
    setToast(null);
    try {
      await invoke<number>("scan_folder", { paths: folders });
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

  /* A project file the operating system handed over — at launch, or while Skimrr was
     already running. The backend puts it aside rather than opening it; this is what
     comes and asks. */
  useEffect(() => {
    const take = () =>
      invoke<string | null>("take_pending_project")
        .then((path) => path && setOpeningProject(path))
        .catch(() => undefined);
    take();
    const pending = listen("project-opened", take);
    return () => {
      pending.then((un) => un()).catch(() => undefined);
    };
  }, []);

  /* Everything a finished scan does to get the interface onto the results, reused for a
     project that arrived as a file: the two produce the same state, and having one path
     for both is what stops them drifting apart. */
  const showProject = useCallback(async () => {
    setPickedDays(new Set());
    setDays(await invoke<Day[]>("days").catch(() => []));
    const next = await refresh(simThreshold);
    const scores = next.photos
      .map((p) => p.blur)
      .filter((b): b is number => b !== null);
    setBlurThreshold(blurRangeFor(scores).threshold);
    setTab("days");
    setScreen("results");
  }, [refresh, simThreshold]);

  /* The dialog reports what came across and what did not; by the time it is dismissed
     the backend already holds the imported project, and this is what puts it on screen.
     `lastFolders` is cleared because "scan that folder again" would now be about a
     folder this project has nothing to do with. */
  const onImported = useCallback(() => {
    setOpeningProject(null);
    setLastFolders(null);
    localStorage.removeItem("skimrr-last-folders");
    showProject().catch(() => setError("scan"));
  }, [showProject]);

  async function openProjectFile() {
    const picked = await chooseProjectFile();
    if (picked) setOpeningProject(picked);
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
          const dropped = event.payload.paths ?? [];
          if (dropped.length > 0 && screenRef.current !== "scanning") void scanFolders(dropped);
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

  /* Two destinations, because there are two libraries. What was walked from disk goes
     into Skimrr's own reversible trash; what belongs to Photos is deleted by Photos,
     into its own "Recently Deleted". The batch is sorted here, from what it actually
     holds, rather than every caller being asked to keep the two apart. */
  async function confirmTrash(paths: string[]) {
    const batch = pendingTrash ?? [];
    setPendingTrash(null);
    const fromPhotos = new Set(batch.filter((p) => p.library).map((p) => p.path));
    const onDisk = paths.filter((path) => !fromPhotos.has(path));
    const inPhotos = paths.filter((path) => fromPhotos.has(path));

    try {
      let removed = 0;
      let moved: TrashResult | null = null;
      if (onDisk.length > 0) {
        moved = await invoke<TrashResult>("trash_photos", { paths: onDisk });
        removed += moved.count;
      }
      if (inPhotos.length > 0) {
        removed += await invoke<number>("delete_from_photos", { paths: inPhotos });
      }
      await refresh(simThreshold);
      await loadTrash().catch(() => undefined);
      /* One toast for the whole batch, but its undo only ever covers the disk half: the
         batch id belongs to Skimrr's trash and means nothing to Photos. Without a batch
         id the toast shows no undo at all, which is the honest outcome when everything
         removed went to Photos. */
      showToast({ batch_id: moved?.batch_id ?? "", count: removed });
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

  /** Reviewing is free; removing anything is what the licence buys. */
  function requestTrash(photos: Photo[]) {
    if (photos.length === 0) return;
    setError(null);
    if (licence?.activated) setPendingTrash(photos);
    else setAskLicence(photos);
  }

  /** Adds more Source folders to the scan already on screen: re-scans the union of
      the previous folders and the newly picked ones. The scan cache is keyed per
      file (not per folder combination), so everything already analysed is reused —
      only files under the newly added folder(s) actually cost anything. */
  async function addFolders() {
    const picked = await open({ directory: true, multiple: true });
    if (!picked || picked.length === 0) return;
    const union = [...new Set([...(lastFolders ?? []), ...picked])];
    await scanFolders(union);
  }

  /** Drops one Source folder and re-scans the rest — the mirror of addFolders. Never
      leaves the project empty, so the cross is withheld when only one folder remains. */
  async function removeFolder(folder: string) {
    const next = (lastFolders ?? []).filter((f) => f !== folder);
    if (next.length === 0) return;
    await scanFolders(next);
  }

  /** Explicit choice, from the Gallery tab: only the Source scan's own days, never
      the whole library — see the note on `photosDays` for why. */
  async function includePhotosInGallery() {
    if (!libraryPath || days.length === 0) return;
    setPhotosDaysLoading(true);
    setPhotosPhase("reading");
    setPhotosDaysError(false);
    try {
      const result = await invoke<Day[]>("photos_days", {
        libraryPath,
        onlyDates: days.map((d) => d.key),
      });
      // The days appear straight away; whether the button is finished is a separate
      // question, answered below.
      setPhotosDays(result);
      /* The same assets, merged into the scan itself so a loose copy on disk and the
         copy already filed in Photos land in one duplicate group. `refresh` then does
         the regrouping and resets the per-group keeper choices, exactly as it does
         after a rescan — the merge itself deliberately builds no view, so the model
         runs over the proposed groups once rather than twice.

         Its own try: failing to analyse should cost the analysis, never the Gallery
         the days it just loaded. */
      setPhotosPhase("merging");
      try {
        await invoke<number>("include_photos_in_scan", {
          libraryPath,
          onlyDates: days.map((d) => d.key),
        });
        await refresh(simThreshold);
      } catch {
        /* The Gallery still shows the days; nothing was merged. */
      }
      /* Only now. Marking the work done as soon as the days arrived would unmount the
         button — and with it the spinner — at the exact moment the slow half begins,
         leaving the interface silent through the merge and the re-clustering. */
      setPhotosDaysChecked(true);
    } catch {
      setPhotosDays([]);
      setPhotosDaysError(true);
    } finally {
      setPhotosDaysLoading(false);
      setPhotosPhase(null);
    }
  }

  /** Opens a Gallery day's own photos. Source photos are already in `view.photos` —
      no request needed, just a client-side filter by the same day key the card shows.
      A day with no Source photos (Photos-library only) has nothing to filter, so it
      falls back to fetching that one day's detail on demand — still no iCloud
      download, `photos_day_detail` only ever reads already-cached thumbnails. */
  async function openDay(key: string) {
    if (!view) return;
    const title = formatDayLabel(key, lang);
    const sourcePhotos = view.photos.filter((p) => dayKeyOf(p.taken) === key);

    // A day's card can combine Source and Photos-library covers once "Inclure
    // Photos" has matched it — the grid it opens must show the same combined set,
    // not just the Source half, or the two disagree about what that day contains.
    let photosLibraryPhotos: Photo[] = [];
    if (libraryPath && photosDays.some((d) => d.key === key)) {
      try {
        photosLibraryPhotos = await invoke<Photo[]>("photos_day_detail", { libraryPath, date: key });
      } catch {
        // Missing the Photos-side detail is not a reason to hide the Source photos.
      }
    }

    const combined = [...sourcePhotos, ...photosLibraryPhotos];
    if (combined.length === 0) return;
    setViewer(null);
    setDayGrid({ title, photos: combined });
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
  /* The tab's own count is the number of photographs with any finding at all — the
     same population "All" holds, so the badge and the list can never disagree. */
  const badShotCount = view ? view.photos.filter((p) => isBadShot(p.bad_shot)).length : 0;

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
        {screen === "results" && view && view.photos.length > 0 && (
          <button
            className="trash-link"
            onClick={() => setSending(true)}
            title={t("project.send.hint")}
          >
            {t("project.send.link")}
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
          {lastFolders && lastFolders.length > 0 && (
            <button className="btn-quiet" onClick={() => scanFolders(lastFolders)}>
              {lastFolders.length === 1
                ? t("scan.again", {
                    name: lastFolders[0].split("/").pop() || lastFolders[0],
                  })
                : t("scan.againMany", { count: lastFolders.length })}
            </button>
          )}
          <button className="btn-quiet" onClick={openProjectFile}>
            {t("project.open.fromHome")}
          </button>
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
              className={`tab${tab === "badshot" ? " on" : ""}`}
              onClick={() => setTab("badshot")}
            >
              {t("tabs.badshot")}
              <span className="count mono">{badShotCount}</span>
            </button>
            <button
              className={`tab${tab === "addFolders" ? " on" : ""}`}
              onClick={() => setTab("addFolders")}
            >
              {t("tabs.addFolders")}
            </button>

            <button
              className={`tab${tab === "stats" ? " on" : ""}`}
              onClick={() => setTab("stats")}
            >
              {t("stats.tab")}
            </button>
          </nav>

          {error === "trash" && <p className="error">{t("error.trash")}</p>}
          {error === "share" && <p className="error">{t("error.share")}</p>}

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
              photosDays={photosDays}
              canIncludePhotos={!!libraryPath}
              photosIncluded={photosDaysChecked}
              photosLoading={photosDaysLoading}
              photosPhase={photosPhase}
              photosError={photosDaysError}
              onIncludePhotos={includePhotosInGallery}
              onOpenDay={openDay}
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

          {tab === "badshot" && (
            <BadShotTab
              view={view}
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

          {tab === "addFolders" && (
            <div className="add-folders">
              <p className="add-folders-intro">{t("addFolders.intro")}</p>
              <ul className="add-folders-list">
                {(lastFolders ?? []).map((folder) => (
                  <li key={folder} className="add-folders-item">
                    <span className="mono" title={folder}>
                      {folder}
                    </span>
                    {(lastFolders ?? []).length > 1 && (
                      <button
                        className="add-folders-remove"
                        onClick={() => removeFolder(folder)}
                        aria-label={t("addFolders.remove")}
                        title={t("addFolders.remove")}
                      >
                        <CloseIcon />
                      </button>
                    )}
                  </li>
                ))}
              </ul>
              <button className="btn-primary" onClick={addFolders}>
                {t("addFolders.action")}
              </button>
            </div>
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

      {dayGrid && (
        <DayGridModal
          title={dayGrid.title}
          photos={dayGrid.photos}
          onSelect={(at) => setDayViewer({ photos: dayGrid.photos, at })}
          onCompare={setDayCompare}
          onClose={() => setDayGrid(null)}
        />
      )}

      {dayCompare && (
        <CompareView
          photos={dayCompare}
          indices={dayCompare.map((_, i) => i)}
          onClose={() => setDayCompare(null)}
        />
      )}

      {dayViewer && (
        <Lightbox
          photos={dayViewer.photos}
          index={dayViewer.at}
          onIndex={(at) => setDayViewer({ ...dayViewer, at })}
          onClose={() => setDayViewer(null)}
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
      {sending && view && (
        <SendModal
          keptCount={keptPaths(view, blurThreshold).length}
          onEditor={() => {
            setSending(false);
            shareTo(keptPaths(view, blurThreshold)).catch(() => undefined);
          }}
          onExport={() => {
            setSending(false);
            setExporting(true);
          }}
          onClose={() => setSending(false)}
        />
      )}
      {exporting && (
        <ExportModal
          threshold={simThreshold}
          suggestedName={
            (lastFolders && lastFolders.length === 1
              ? lastFolders[0].split(/[\\/]/).pop()
              : null) || "skimrr-project"
          }
          onClose={() => setExporting(false)}
        />
      )}
      {openingProject && (
        <OpenProjectModal
          path={openingProject}
          onClose={() => setOpeningProject(null)}
          onImported={onImported}
        />
      )}
      {toast && (
        <UndoToast
          count={toast.count}
          onUndo={toast.batch_id ? undo : undefined}
        />
      )}
    </div>
  );
}
