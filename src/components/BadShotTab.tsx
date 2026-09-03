import { useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { BadShot, View } from "../types";

/** A finding is shown at or above this confidence — the same bar the Rust side uses. */
const REPORT = 0.5;

/** The four findings, in the order they are offered. */
export const CATEGORIES = ["blur", "closedEyes", "underexposed", "overexposed"] as const;
export type Category = (typeof CATEGORIES)[number];

/** Reads one finding off a verdict. Keeps the mapping in one place so the filters, the
    counts and the badges can never disagree about what "blur" means. */
export function has(bad: BadShot | null | undefined, category: Category): boolean {
  if (!bad) return false;
  const score =
    category === "blur"
      ? bad.blur
      : category === "closedEyes"
        ? bad.closed_eyes
        : category === "underexposed"
          ? bad.underexposed
          : bad.overexposed;
  return score !== null && score !== undefined && score >= REPORT;
}

export function isBadShot(bad: BadShot | null | undefined): boolean {
  return CATEGORIES.some((c) => has(bad, c));
}

function BlurIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round">
      <circle cx="12" cy="12" r="3.2" />
      <circle cx="12" cy="12" r="7.4" strokeDasharray="2 3" />
    </svg>
  );
}

function EyeIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 13.5c3-4 6-6 9-6s6 2 9 6" />
      <path d="M6 17l1.6-2.4M12 18.2V15.4M18 17l-1.6-2.4" />
    </svg>
  );
}

function DarkIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9">
      <circle cx="12" cy="12" r="7.5" />
      <path d="M12 4.5a7.5 7.5 0 0 0 0 15z" fill="currentColor" stroke="none" />
    </svg>
  );
}

function BrightIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round">
      <circle cx="12" cy="12" r="4" />
      <path d="M12 2.6v2.2M12 19.2v2.2M2.6 12h2.2M19.2 12h2.2M5.3 5.3l1.6 1.6M17.1 17.1l1.6 1.6M18.7 5.3l-1.6 1.6M6.9 17.1l-1.6 1.6" />
    </svg>
  );
}

export function CategoryIcon({ category }: { category: Category }) {
  if (category === "blur") return <BlurIcon />;
  if (category === "closedEyes") return <EyeIcon />;
  if (category === "underexposed") return <DarkIcon />;
  return <BrightIcon />;
}

/**
 * The photographs this scan thinks are probably spoiled, and why.
 *
 * One list with a filter rather than four lists: a photograph that is both blurred and
 * has someone blinking is one photograph, and showing it twice in "All" would make the
 * count meaningless. The filters narrow the same list instead, and every thumbnail
 * carries a badge per finding so the reason survives the narrowing.
 */
export function BadShotTab({
  view,
  selected,
  onToggle,
  onSelectAll,
  onClear,
  onTrashSelected,
  onExpand,
}: {
  view: View;
  selected: Set<number>;
  onToggle: (photoIndex: number) => void;
  onSelectAll: (photoIndices: number[]) => void;
  onClear: () => void;
  onTrashSelected: () => void;
  onExpand: (photoIndices: number[], position: number) => void;
}) {
  const { t } = useTranslation();
  const [filter, setFilter] = useState<Category | "all">("all");

  const { flagged, counts } = useMemo(() => {
    const flagged = view.photos
      .map((photo, index) => ({ photo, index }))
      .filter(({ photo }) => isBadShot(photo.bad_shot))
      // Worst first: whatever the strongest finding on each photograph is.
      .sort((a, b) => worst(b.photo.bad_shot) - worst(a.photo.bad_shot));

    const counts: Record<Category | "all", number> = {
      all: flagged.length,
      blur: 0,
      closedEyes: 0,
      underexposed: 0,
      overexposed: 0,
    };
    for (const { photo } of flagged) {
      for (const c of CATEGORIES) {
        if (has(photo.bad_shot, c)) counts[c] += 1;
      }
    }
    return { flagged, counts };
  }, [view.photos]);

  const shown = filter === "all" ? flagged : flagged.filter(({ photo }) => has(photo.bad_shot, filter));

  function cell({ photo, index }: (typeof flagged)[number], pos: number) {
    const found = CATEGORIES.filter((c) => has(photo.bad_shot, c));
    return (
      <button
        key={photo.path}
        className={`cell${selected.has(index) ? " sel" : ""}`}
        onClick={() => onToggle(index)}
        title={`${photo.name} — ${found.map((c) => t(`badshot.${c}`)).join(", ")}`}
      >
        <img
          src={convertFileSrc(photo.thumb ?? photo.preview)}
          alt={photo.name}
          loading="lazy"
          decoding="async"
        />
        {/* One badge per finding, so a photograph filtered under "Blur" still shows
            that someone also blinked in it. */}
        <span className="badges" aria-hidden="true">
          {found.map((c) => (
            <span key={c} className="badge-flag" title={t(`badshot.${c}`)}>
              <CategoryIcon category={c} />
            </span>
          ))}
        </span>
        {selected.has(index) && <span className="check">✓</span>}
        <span
          className="expand"
          role="button"
          tabIndex={0}
          aria-label={t("viewer.open")}
          title={t("viewer.open")}
          onClick={(e) => {
            e.stopPropagation();
            onExpand(shown.map((b) => b.index), pos);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              e.stopPropagation();
              onExpand(shown.map((b) => b.index), pos);
            }
          }}
        >
          ⤢
        </span>
      </button>
    );
  }

  if (flagged.length === 0) {
    return (
      <div className="empty">
        <h2>{t("badshot.none")}</h2>
      </div>
    );
  }

  return (
    <>
      <div className="badshot-filters" role="tablist" aria-label={t("badshot.tab")}>
        {(["all", ...CATEGORIES] as const).map((c) => (
          <button
            key={c}
            role="tab"
            aria-selected={filter === c}
            className={`badshot-filter${filter === c ? " on" : ""}`}
            /* A category nobody's photographs fall into is left visible but inert,
               rather than hidden: its absence is itself the answer, and a row of
               filters that changes shape between folders is harder to read. */
            disabled={counts[c] === 0}
            onClick={() => setFilter(c)}
          >
            {c !== "all" && <CategoryIcon category={c} />}
            <span>{t(`badshot.${c}`)}</span>
            <span className="badshot-count mono">{counts[c]}</span>
          </button>
        ))}
      </div>

      <div className="blur-actions">
        <button
          className="btn-ghost"
          onClick={() => onSelectAll(shown.map((b) => b.index))}
          disabled={shown.length === 0}
        >
          {t("blur.selectAll")}
        </button>
        {selected.size > 0 && (
          <button className="btn-ghost" onClick={onClear}>
            {t("blur.clear")}
          </button>
        )}
        <span className="spacer" />
        <button className="btn-danger" disabled={selected.size === 0} onClick={onTrashSelected}>
          {t("blur.trash", { count: selected.size })}
        </button>
      </div>

      <div className="blur-grid">{shown.map((b, i) => cell(b, i))}</div>
    </>
  );
}

/**
 * The findings on one photograph, spelled out where it is being judged full-screen.
 *
 * Says how sure rather than how much: a percentage invites arithmetic nobody wants to
 * do at the moment of deciding whether to delete a photograph. Closed eyes are the one
 * exception, where "1 face of 3" changes the decision and a bare label would not.
 */
export function BadShotPanel({ bad }: { bad: BadShot | null | undefined }) {
  const { t } = useTranslation();
  const found = CATEGORIES.filter((c) => has(bad, c));
  if (found.length === 0 || !bad) return null;

  const score = (c: Category) =>
    c === "blur" ? bad.blur : c === "closedEyes" ? bad.closed_eyes
      : c === "underexposed" ? bad.underexposed : bad.overexposed;

  return (
    <div className="badshot-panel">
      {found.map((c) => (
        <div key={c} className="badshot-finding">
          <CategoryIcon category={c} />
          <strong>{t(`badshot.${c}`)}</strong>
          <span className="badshot-detail">
            {c === "closedEyes" && bad.faces_closed
              ? `${t("badshot.faces", { count: bad.faces_closed })} · ${Math.round((score(c) ?? 0) * 100)}%`
              : `${(score(c) ?? 0) >= 0.75 ? t("badshot.high") : t("badshot.medium")} ${t("badshot.confidence")}`}
          </span>
        </div>
      ))}
    </div>
  );
}

/** The strongest finding on a verdict, for ordering. */
function worst(bad: BadShot | null | undefined): number {
  if (!bad) return 0;
  return Math.max(
    bad.blur ?? 0,
    bad.closed_eyes ?? 0,
    bad.underexposed ?? 0,
    bad.overexposed ?? 0,
  );
}
