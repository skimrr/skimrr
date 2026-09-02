import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { View, formatBytes, formatDate } from "../types";
import { PhotoImage } from "./PhotoImage";

type SortOrder = "asc" | "desc";

function SortIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 19V5M12 5 7 10M12 5l5 5" />
    </svg>
  );
}

/** Earliest shot in the group stands in for "the group's date" — a duplicate group is
    near-enough one moment that picking a single representative photo (rather than
    averaging) is both simpler and never misleading. */
function groupDate(view: View, groupIndex: number): number {
  const group = view.groups[groupIndex];
  return Math.min(...group.indices.map((i) => view.photos[i].taken));
}

export function DuplicatesTab({
  view,
  kept,
  onKeep,
  simThreshold,
  onSimThreshold,
  onTrashGroup,
  onCompare,
  onExpand,
  cursor,
}: {
  view: View;
  kept: number[];
  onKeep: (groupIndex: number, position: number) => void;
  simThreshold: number;
  onSimThreshold: (value: number) => void;
  onTrashGroup: (groupIndex: number) => void;
  onCompare: (groupIndex: number) => void;
  onExpand: (groupIndex: number, position: number) => void;
  /** Group and photo the keyboard is on, or null when the pointer is driving. */
  cursor: { group: number; pos: number } | null;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;
  const [sortOrder, setSortOrder] = useState<SortOrder>("asc");

  // Sorts a list of the groups' own indices, not the groups themselves: every
  // downstream lookup (kept[gi], cursor.group, onKeep(gi, ...)) is keyed by a
  // group's position in view.groups, which must stay stable regardless of the
  // order they're displayed in.
  const order = useMemo(() => {
    const idx = view.groups.map((_, i) => i);
    idx.sort((a, b) => groupDate(view, a) - groupDate(view, b));
    if (sortOrder === "desc") idx.reverse();
    return idx;
  }, [view, sortOrder]);

  return (
    <>
      <div className="toolbar">
        <label className="toolbar-label" htmlFor="sim-slider">
          {t("dup.threshold")}
        </label>
        <input
          id="sim-slider"
          type="range"
          min={0}
          max={32}
          step={1}
          value={simThreshold}
          onChange={(e) => onSimThreshold(Number(e.target.value))}
        />
        <span className="toolbar-value mono">
          ~{Math.round(((128 - simThreshold) / 128) * 100)} %
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
      </div>

      {view.groups.length === 0 ? (
        <div className="empty">
          <h2>{t("results.noneTitle")}</h2>
          <p>{t("results.noneBody", { count: view.total_files })}</p>
        </div>
      ) : (
        order.map((gi, displayPos) => {
          const group = view.groups[gi];
          const trashCount = group.indices.length - 1;
          return (
            <section
              className="group-card"
              key={group.indices.join("-")}
              data-group={gi}
            >
              <div className="group-label">
                {t("group.title", { n: displayPos + 1, total: view.groups.length })}
                {" · "}
                {group.kind === "pair"
                  ? t("group.pair")
                  : group.kind === "exact"
                    ? t("group.exact")
                    : t("group.similar", { p: group.similarity })}
              </div>
              <div className="group-grid">
                {group.indices.map((photoIndex, pos) => {
                  const photo = view.photos[photoIndex];
                  return (
                    <button
                      key={photo.path}
                      className={`photo-card${kept[gi] === pos ? " keep" : ""}${
                        cursor && cursor.group === gi && cursor.pos === pos
                          ? " cursor"
                          : ""
                      }`}
                      data-cursor={
                        cursor && cursor.group === gi && cursor.pos === pos
                          ? "true"
                          : undefined
                      }
                      onClick={() => onKeep(gi, pos)}
                    >
                      <span className="thumb">
                        <PhotoImage photo={photo} />
                        {kept[gi] === pos && (
                          <span className="badge">{t("group.kept")}</span>
                        )}
                        {/* Says where the file lives, which is why it is never the one
                            offered up: Skimrr can compare against a Photos asset but
                            cannot move it. */}
                        {photo.library && (
                          <span className="badge badge-library">{t("group.inLibrary")}</span>
                        )}
                        <span
                          className="expand"
                          role="button"
                          tabIndex={0}
                          aria-label={t("viewer.open")}
                          title={t("viewer.open")}
                          onClick={(e) => {
                            e.stopPropagation();
                            onExpand(gi, pos);
                          }}
                          onKeyDown={(e) => {
                            if (e.key === "Enter" || e.key === " ") {
                              e.preventDefault();
                              e.stopPropagation();
                              onExpand(gi, pos);
                            }
                          }}
                        >
                          ⤢
                        </span>
                      </span>
                      <span className="meta mono">
                        <span className="name" title={photo.path}>
                          {photo.name}
                        </span>
                        <span>
                          {photo.kind ??
                            (photo.width > 0
                              ? `${photo.width} × ${photo.height}`
                              : "")}
                        </span>
                      </span>
                      <span className="meta mono">
                        <span>{formatDate(photo.taken, lang)}</span>
                        <span>{formatBytes(photo.size, lang)}</span>
                      </span>
                    </button>
                  );
                })}
              </div>
              <div className="group-foot">
                <p className="group-hint">
                  {group.reason && kept[gi] === group.suggested
                    ? t("group.keptWhy", { why: t(`group.why.${group.reason}`) })
                    : t("group.hint")}
                </p>
                <button className="btn-ghost" onClick={() => onCompare(gi)}>
                  {t("compare.open")}
                </button>
                <button className="btn-danger" onClick={() => onTrashGroup(gi)}>
                  {t("group.trash", { count: trashCount })}
                </button>
              </div>
            </section>
          );
        })
      )}
    </>
  );
}
