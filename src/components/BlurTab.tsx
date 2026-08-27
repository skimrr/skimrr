import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { View } from "../types";

/** How far above the cut a photo can sit and still be worth showing. */
const BORDERLINE_FACTOR = 2.5;
const BORDERLINE_MAX = 12;

export function BlurTab({
  view,
  threshold,
  max,
  median,
  onThreshold,
  selected,
  onToggle,
  onSelectAll,
  onClear,
  onTrashSelected,
  onExpand,
}: {
  view: View;
  threshold: number;
  max: number;
  /** Typical sharpness for this folder, used as the meter's full mark. */
  median: number;
  onThreshold: (value: number) => void;
  selected: Set<number>;
  onToggle: (photoIndex: number) => void;
  onSelectAll: (photoIndices: number[]) => void;
  onClear: () => void;
  onTrashSelected: () => void;
  onExpand: (photoIndices: number[], position: number) => void;
}) {
  const { t } = useTranslation();

  const scored = view.photos
    .map((photo, index) => ({ photo, index }))
    .filter(({ photo }) => photo.blur !== null)
    .sort((a, b) => (a.photo.blur ?? 0) - (b.photo.blur ?? 0));

  const blurry = scored.filter(({ photo }) => (photo.blur ?? 0) < threshold);
  /* The hard part of a threshold is not seeing what sits just outside it, so the next
     few photos above the cut stay on screen, dimmed but one click away. */
  const borderline = scored
    .filter(
      ({ photo }) =>
        (photo.blur ?? 0) >= threshold &&
        (photo.blur ?? 0) < threshold * BORDERLINE_FACTOR,
    )
    .slice(0, BORDERLINE_MAX);

  const all = [...blurry, ...borderline];

  function cell({ photo, index }: (typeof scored)[number], pos: number) {
    const score = photo.blur ?? 0;
    const fill = Math.max(0.02, Math.min(1, median > 0 ? score / median : 0));
    return (
      <button
        key={photo.path}
        className={`cell${selected.has(index) ? " sel" : ""}`}
        onClick={() => onToggle(index)}
        title={`${photo.name}, ${t("viewer.sharpness")} ${Math.round(score)}`}
      >
        <img
          src={convertFileSrc(photo.thumb ?? photo.preview)}
          alt={photo.name}
          loading="lazy"
          decoding="async"
        />
        <span className="meter" aria-hidden="true">
          <i style={{ width: `${fill * 100}%` }} />
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
            onExpand(
              all.map((b) => b.index),
              pos,
            );
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              e.stopPropagation();
              onExpand(
                all.map((b) => b.index),
                pos,
              );
            }
          }}
        >
          ⤢
        </span>
      </button>
    );
  }

  return (
    <>
      <div className="toolbar">
        <label className="toolbar-label" htmlFor="blur-slider">
          {t("blur.threshold")}
        </label>
        <input
          id="blur-slider"
          type="range"
          min={0}
          max={max}
          step={Math.max(max / 200, 0.0001)}
          value={threshold}
          onChange={(e) => onThreshold(Number(e.target.value))}
        />
        <span className="toolbar-value">
          {t("blur.below", { count: blurry.length })}
        </span>
      </div>

      {blurry.length === 0 && borderline.length === 0 ? (
        <div className="empty">
          <h2>{t("blur.none")}</h2>
        </div>
      ) : (
        <>
          <div className="blur-actions">
            <button
              className="btn-ghost"
              onClick={() => onSelectAll(blurry.map((b) => b.index))}
              disabled={blurry.length === 0}
            >
              {t("blur.selectAll")}
            </button>
            {selected.size > 0 && (
              <button className="btn-ghost" onClick={onClear}>
                {t("blur.clear")}
              </button>
            )}
            <span className="spacer" />
            <button
              className="btn-danger"
              disabled={selected.size === 0}
              onClick={onTrashSelected}
            >
              {t("blur.trash", { count: selected.size })}
            </button>
          </div>

          {blurry.length > 0 && (
            <div className="blur-grid">{blurry.map((b, i) => cell(b, i))}</div>
          )}

          {borderline.length > 0 && (
            <>
              <p className="borderline-label">{t("blur.borderline")}</p>
              <div className="blur-grid borderline">
                {borderline.map((b, i) => cell(b, blurry.length + i))}
              </div>
            </>
          )}
        </>
      )}
    </>
  );
}
