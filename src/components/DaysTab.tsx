import { useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { formatBytes } from "../types";

export interface Day {
  key: string;
  count: number;
  bytes: number;
  covers: string[];
}

/** One calendar day, after merging the Source scan's own days with whatever the
    Destination library (Photos) already has for that same day. */
interface MergedDay {
  key: string;
  covers: string[];
  source: Day | null;
  photosCount: number;
}

type SortOrder = "asc" | "desc";

function mergeDays(days: Day[], photosDays: Day[], order: SortOrder): MergedDay[] {
  const photosByKey = new Map(photosDays.map((d) => [d.key, d]));
  const keys = new Set([...days.map((d) => d.key), ...photosDays.map((d) => d.key)]);
  const sorted = [...keys].sort();
  if (order === "desc") sorted.reverse();

  return sorted.map((key) => {
    const source = days.find((d) => d.key === key) ?? null;
    const photos = photosByKey.get(key) ?? null;
    // At least one from each side when both exist, so a day never reads as
    // "only in Photos" or "only in the source" when it is really both.
    const covers =
      source && photos
        ? [...source.covers.slice(0, 2), ...photos.covers.slice(0, 2)].slice(0, 4)
        : (source ?? photos)?.covers.slice(0, 4) ?? [];
    return { key, covers, source, photosCount: photos?.count ?? 0 };
  });
}

function SortIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 19V5M12 5 7 10M12 5l5 5" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
      <path d="M5 12.5 10 17.5 19 7" />
    </svg>
  );
}

/* A generic photo/frame glyph, not any particular app's icon — third-party use of
   Apple's own Photos icon needs a written trademark license (confirmed against
   Apple's own guidelines), which this sidesteps entirely by not being it. */
function PhotosIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
      <rect x="3.5" y="4.5" width="17" height="15" rx="2.5" />
      <circle cx="9" cy="10" r="1.6" />
      <path d="M4.5 17 9.5 12l3 3 4-4.5 3 3.5" />
    </svg>
  );
}

/**
 * The folder as its owner remembers it: by the day the photographs were taken.
 *
 * Clicking a day opens its photos in the viewer. Selecting a day — which narrows the
 * Duplicate and Blur tabs to it, the thing that makes a folder of several thousand
 * photographs workable — is a separate action, the small check mark on the card:
 * looking and picking used to be the same click, which meant there was no way to just
 * glance at a day without also selecting it.
 *
 * `photosDays` (optional) adds the Destination library's own days alongside the
 * Source's, scoped by the caller to exactly the Source's own dates — never the whole
 * library's history — so a day already fully covered by Photos is visible without
 * switching away. Bringing that in at all is the viewer's explicit choice
 * (`onIncludePhotos`), not automatic. A day with no Source photos (Photos-library
 * only) is still openable — there is simply nothing here to narrow the Duplicate/Blur
 * tabs to, so it gets no check mark.
 */
export function DaysTab({
  days,
  photosDays = [],
  canIncludePhotos = false,
  photosIncluded = false,
  photosLoading = false,
  photosError = false,
  onIncludePhotos,
  onOpenDay,
  selected,
  onToggle,
  onClear,
}: {
  days: Day[];
  photosDays?: Day[];
  canIncludePhotos?: boolean;
  photosIncluded?: boolean;
  photosLoading?: boolean;
  photosError?: boolean;
  onIncludePhotos?: () => void;
  onOpenDay: (key: string) => void;
  selected: Set<string>;
  onToggle: (key: string) => void;
  onClear: () => void;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;
  const [sortOrder, setSortOrder] = useState<SortOrder>("asc");

  const merged = useMemo(
    () => mergeDays(days, photosDays, sortOrder),
    [days, photosDays, sortOrder],
  );

  const label = (key: string) =>
    key
      ? new Intl.DateTimeFormat(lang, { dateStyle: "full" }).format(new Date(`${key}T12:00:00`))
      : t("days.undated");

  return (
    <div className="days">
      <div className="days-bar">
        <span className="days-hint">
          {selected.size > 0
            ? t("days.selected", { count: selected.size })
            : t("days.hint")}
        </span>
        <span className="spacer" />
        <button
          type="button"
          className={`btn-ghost btn-sort${sortOrder === "desc" ? " desc" : ""}`}
          onClick={() => setSortOrder((o) => (o === "asc" ? "desc" : "asc"))}
          title={t("days.sortLabel")}
        >
          <SortIcon />
          {sortOrder === "asc" ? t("days.sortAsc") : t("days.sortDesc")}
        </button>
        {canIncludePhotos && !photosIncluded && (
          <button className="btn-ghost btn-photos" onClick={onIncludePhotos} disabled={photosLoading}>
            <PhotosIcon />
            {photosLoading ? t("days.includingPhotos") : t("days.includePhotos")}
          </button>
        )}
        {selected.size > 0 && (
          <button className="btn-ghost" onClick={onClear}>
            {t("days.clear")}
          </button>
        )}
      </div>

      {photosError && <p className="error">{t("days.photosUnreadable")}</p>}

      <ul className="day-list">
        {merged.map(({ key, covers, source, photosCount }) => {
          const on = source !== null && selected.has(key);
          const facts = source ? (
            <span className="day-facts mono">
              {source.count} {t("days.photos")} · {formatBytes(source.bytes, lang)}
              {photosCount > 0 && ` · ${t("days.alsoInPhotos", { count: photosCount })}`}
            </span>
          ) : (
            <span className="day-facts mono">{t("days.onlyInPhotos", { count: photosCount })}</span>
          );

          return (
            <li key={key}>
              <div className={`day${on ? " on" : ""}${source ? "" : " day-photos-only"}`}>
                <button className="day-open" onClick={() => onOpenDay(key)}>
                  <span className="day-covers">
                    {covers.map((cover) => (
                      <img key={cover} src={convertFileSrc(cover)} alt="" loading="lazy" decoding="async" />
                    ))}
                  </span>
                  <span className="day-meta">
                    <span className="day-label">{label(key)}</span>
                    {facts}
                  </span>
                </button>
                {source && (
                  <button
                    className={`day-check${on ? " on" : ""}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      onToggle(key);
                    }}
                    aria-pressed={on}
                    aria-label={t("days.select")}
                    title={t("days.select")}
                  >
                    <CheckIcon />
                  </button>
                )}
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
