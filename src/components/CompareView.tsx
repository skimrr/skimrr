import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Photo, formatBytes } from "../types";

const MAX_SCALE = 8;

/**
 * Side by side, with one zoom shared by every pane.
 *
 * Four frames of the same burst look identical at fit-to-window; what separates them is
 * whether the eyes are sharp, and that only shows at full pixels. So the scale and the
 * offset live here rather than in each pane: zooming into one face zooms into all of
 * them, at the same point, which is the only way to answer "which of these is sharp".
 */
export function CompareView({
  photos,
  indices,
  kept,
  onKeep,
  onClose,
}: {
  photos: Photo[];
  /** Positions within the group, in display order. */
  indices: number[];
  /** Position of the photo currently marked as the keeper, when there is one — a
      plain look-side-by-side session (e.g. from the Gallery) has no keeper at all. */
  kept?: number;
  onKeep?: (position: number) => void;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;

  const [perView, setPerView] = useState<2 | 4>(indices.length >= 4 ? 4 : 2);
  const [page, setPage] = useState(0);
  const [focus, setFocus] = useState(0);
  const [scale, setScale] = useState(1);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const [details, setDetails] = useState<Record<string, string>>({});
  const dragging = useRef<{ x: number; y: number } | null>(null);

  const start = page * perView;
  const shown = indices.slice(start, start + perView);
  const pages = Math.max(1, Math.ceil(indices.length / perView));

  const reset = useCallback(() => {
    setScale(1);
    setOffset({ x: 0, y: 0 });
  }, []);

  /* The grid renditions are sized for the grid. Comparing at full pixels needs the
     detail rendition, built on demand and swapped in per pane as it arrives. */
  useEffect(() => {
    let cancelled = false;
    for (const position of shown) {
      const photo = photos[position];
      if (!photo || details[photo.path]) continue;
      invoke<string>("detail_preview", { path: photo.path })
        .then((detail) => {
          if (!cancelled && detail) {
            setDetails((d) => ({ ...d, [photo.path]: detail }));
          }
        })
        .catch(() => undefined);
    }
    return () => {
      cancelled = true;
    };
  }, [shown.join(","), photos, details]);

  const zoomBy = useCallback((factor: number) => {
    setScale((s) => Math.min(MAX_SCALE, Math.max(1, s * factor)));
  }, []);

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      const el = document.activeElement as HTMLElement | null;
      if (el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA")) return;
      switch (e.key) {
        case "Escape":
          onClose();
          break;
        case "ArrowRight":
          setFocus((f) => Math.min(f + 1, shown.length - 1));
          break;
        case "ArrowLeft":
          setFocus((f) => Math.max(f - 1, 0));
          break;
        case "Enter":
          if (onKeep && shown[focus] !== undefined) onKeep(shown[focus]);
          break;
        case "+":
        case "=":
          zoomBy(1.4);
          break;
        case "-":
          zoomBy(1 / 1.4);
          break;
        case "0":
          reset();
          break;
        default:
          return;
      }
      e.preventDefault();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [focus, shown, onKeep, onClose, zoomBy, reset]);

  return (
    <div className="compare-view" role="dialog" aria-modal="true">
      <header className="compare-bar">
        <span className="compare-count mono">
          {indices.length} {t("compare.photos")}
        </span>
        <span className="spacer" />
        <div className="compare-layout" role="group" aria-label={t("compare.layout")}>
          {([2, 4] as const).map((n) => (
            <button
              key={n}
              className={`chip${perView === n ? " on" : ""}`}
              onClick={() => {
                setPerView(n);
                setPage(0);
                setFocus(0);
              }}
              disabled={indices.length < n}
            >
              {n}
            </button>
          ))}
        </div>
        <button className="chip" onClick={() => zoomBy(1 / 1.4)} aria-label={t("compare.out")}>
          −
        </button>
        <span className="compare-zoom mono">{Math.round(scale * 100)} %</span>
        <button className="chip" onClick={() => zoomBy(1.4)} aria-label={t("compare.in")}>
          +
        </button>
        <button className="chip" onClick={reset}>
          {t("compare.fit")}
        </button>
        <button className="chip" onClick={onClose}>
          {t("actions.close")}
        </button>
      </header>

      <div
        className={`compare-grid cols-${perView}`}
        onPointerDown={(e) => {
          dragging.current = { x: e.clientX - offset.x, y: e.clientY - offset.y };
          (e.target as HTMLElement).setPointerCapture?.(e.pointerId);
        }}
        onPointerMove={(e) => {
          if (!dragging.current) return;
          setOffset({ x: e.clientX - dragging.current.x, y: e.clientY - dragging.current.y });
        }}
        onPointerUp={() => {
          dragging.current = null;
        }}
        onWheel={(e) => zoomBy(e.deltaY < 0 ? 1.12 : 1 / 1.12)}
      >
        {shown.map((position, slot) => {
          const photo = photos[position];
          if (!photo) return null;
          const src = details[photo.path] ?? photo.preview;
          return (
            <button
              key={photo.path}
              className={`pane${slot === focus ? " focused" : ""}${
                position === kept ? " keep" : ""
              }`}
              onClick={() => setFocus(slot)}
              onDoubleClick={() => onKeep?.(position)}
            >
              <span className="pane-stage">
                <img
                  src={convertFileSrc(src)}
                  alt={photo.name}
                  draggable={false}
                  style={{
                    transform: `translate(${offset.x}px, ${offset.y}px) scale(${scale})`,
                  }}
                />
              </span>
              <span className="pane-foot">
                <span className="mono">{photo.name}</span>
                <span className="mono pane-size">{formatBytes(photo.size, lang)}</span>
              </span>
              {position === kept && <span className="pane-badge">{t("group.kept")}</span>}
            </button>
          );
        })}
      </div>

      <footer className="compare-foot">
        {pages > 1 && (
          <div className="compare-pages">
            <button
              className="chip"
              onClick={() => {
                setPage((p) => Math.max(0, p - 1));
                setFocus(0);
              }}
              disabled={page === 0}
            >
              ‹
            </button>
            <span className="mono">
              {page + 1} / {pages}
            </span>
            <button
              className="chip"
              onClick={() => {
                setPage((p) => Math.min(pages - 1, p + 1));
                setFocus(0);
              }}
              disabled={page >= pages - 1}
            >
              ›
            </button>
          </div>
        )}
        <span className="spacer" />
        <span className="compare-hint">{t(onKeep ? "compare.hint" : "compare.hintPlain")}</span>
        {onKeep && (
          <button
            className="btn-primary"
            onClick={() => shown[focus] !== undefined && onKeep(shown[focus])}
          >
            {t("compare.keep")}
          </button>
        )}
      </footer>
    </div>
  );
}
