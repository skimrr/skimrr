import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { View, formatBytes, formatDate } from "../types";

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
      </div>

      {view.groups.length === 0 ? (
        <div className="empty">
          <h2>{t("results.noneTitle")}</h2>
          <p>{t("results.noneBody", { count: view.total_files })}</p>
        </div>
      ) : (
        view.groups.map((group, gi) => {
          const trashCount = group.indices.length - 1;
          return (
            <section
              className="group-card"
              key={group.indices.join("-")}
              data-group={gi}
            >
              <div className="group-label">
                {t("group.title", { n: gi + 1, total: view.groups.length })}
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
                        <img
                          src={convertFileSrc(photo.preview)}
                          alt={photo.name}
                          loading="lazy"
                          decoding="async"
                        />
                        {kept[gi] === pos && (
                          <span className="badge">{t("group.kept")}</span>
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
