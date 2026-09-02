import { useCallback, useEffect, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Photo, formatBytes, formatDate } from "../types";

const MAX_SCALE = 8;

/* Judging a duplicate or a soft frame needs the picture big, so the viewer fills the
   window, steps through the group it was opened from, and zooms into the grain. */
export function Lightbox({
  photos,
  index,
  onIndex,
  onClose,
  onKeep,
  isKept,
}: {
  photos: Photo[];
  index: number;
  onIndex: (next: number) => void;
  onClose: () => void;
  /** Offered only when the viewer was opened from a duplicate group. */
  onKeep?: (index: number) => void;
  isKept?: boolean;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;
  const photo = photos[index];
  const many = photos.length > 1;

  const [scale, setScale] = useState(1);
  const [offset, setOffset] = useState({ x: 0, y: 0 });
  const [src, setSrc] = useState(() => convertFileSrc(photo.preview));
  /* Layout width at rest and the file's real pixel width, so the readout can say how
     far past the actual pixels the view is. Past 100% there is nothing left to see. */
  const [sizes, setSizes] = useState({ layout: 0, natural: 0 });
  const stageRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);
  const dragging = useRef<{ x: number; y: number } | null>(null);

  const reset = useCallback(() => {
    setScale(1);
    setOffset({ x: 0, y: 0 });
  }, []);

  /* Renditions in the grid are sized for the grid. Zooming wants everything the file
     can give, so the full-size one is built on demand and swapped in.

     For a library photo that means asking Photos to export the original, which with
     iCloud storage optimised downloads it first — a few seconds, once per photo, then
     cached. Without it a 480x360 derivative would stand in for a 16 megapixel frame,
     and side by side in a duplicate group the better copy looks like the worse one.
     Deliberately only here, where one photo has been opened on purpose: never during a
     scan, which would pull an entire library across the network. */
  useEffect(() => {
    let cancelled = false;
    setSrc(convertFileSrc(photo.preview));
    reset();
    (photo.library
      ? invoke<string>("library_original", { path: photo.path, thumb: false })
      : invoke<string>("detail_preview", { path: photo.path }))
      .then((detail) => {
        if (!cancelled && detail) setSrc(convertFileSrc(detail));
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [photo.path, photo.preview, reset]);

  const step = useCallback(
    (delta: number) => onIndex((index + delta + photos.length) % photos.length),
    [index, photos.length, onIndex],
  );

  /* 1:1 is the photographer's check for focus: one image pixel on one screen pixel. */
  const nativeScale =
    sizes.layout > 0 && sizes.natural > 0 ? sizes.natural / sizes.layout : 1;
  const percent =
    sizes.layout > 0 && sizes.natural > 0
      ? Math.round((sizes.layout * scale * 100) / sizes.natural)
      : Math.round(scale * 100);

  const zoomBy = useCallback((factor: number, origin?: { x: number; y: number }) => {
    setScale((current) => {
      const next = Math.min(MAX_SCALE, Math.max(1, current * factor));
      if (next === 1) {
        setOffset({ x: 0, y: 0 });
      } else if (origin && stageRef.current) {
        // Keep whatever sits under the pointer pinned while the scale changes.
        const box = stageRef.current.getBoundingClientRect();
        const px = origin.x - box.left - box.width / 2;
        const py = origin.y - box.top - box.height / 2;
        const ratio = next / current;
        setOffset((o) => ({
          x: px - (px - o.x) * ratio,
          y: py - (py - o.y) * ratio,
        }));
      }
      return next;
    });
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      if (e.key === "ArrowRight" && many) step(1);
      if (e.key === "ArrowLeft" && many) step(-1);
      if (e.key === "+" || e.key === "=") zoomBy(1.4);
      if (e.key === "-") zoomBy(1 / 1.4);
      if (e.key === "0") reset();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, step, many, zoomBy, reset]);

  // Wheel zoom has to be a non-passive listener to stop the page reacting too.
  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      // Gentle enough that a single notch is a step, not a leap.
      zoomBy(Math.exp(-e.deltaY / 500), { x: e.clientX, y: e.clientY });
    };
    stage.addEventListener("wheel", onWheel, { passive: false });
    return () => stage.removeEventListener("wheel", onWheel);
  }, [zoomBy]);

  if (!photo) return null;

  const zoomed = scale > 1;

  return (
    <div
      className="lightbox"
      role="dialog"
      aria-modal="true"
      aria-label={photo.name}
    >
      <div className="lightbox-bar">
        <span className="lightbox-name mono">{photo.name}</span>
        {many && (
          <span className="lightbox-counter mono">
            {index + 1} / {photos.length}
          </span>
        )}
        <span className="spacer" />
        <div className="zoom-group">
          <button
            onClick={() => zoomBy(1 / 1.4)}
            disabled={!zoomed}
            aria-label={t("viewer.zoomOut")}
            title={t("viewer.zoomOut")}
          >
            −
          </button>
          <button
            className="zoom-level mono"
            onClick={() =>
              scale === 1 ? setScale(Math.min(MAX_SCALE, nativeScale)) : reset()
            }
            title={scale === 1 ? t("viewer.actual") : t("viewer.fit")}
          >
            {percent} %
          </button>
          <button
            onClick={() => zoomBy(1.4)}
            disabled={scale >= MAX_SCALE}
            aria-label={t("viewer.zoomIn")}
            title={t("viewer.zoomIn")}
          >
            +
          </button>
        </div>
        {onKeep && (
          <button
            className={`lightbox-keep${isKept ? " on" : ""}`}
            onClick={() => onKeep(index)}
            disabled={isKept}
          >
            {isKept ? t("group.kept") : t("viewer.keep")}
          </button>
        )}
        <button
          className="lightbox-close"
          onClick={onClose}
          aria-label={t("viewer.close")}
          title={t("viewer.close")}
        >
          ✕
        </button>
      </div>

      <div
        className={`lightbox-stage${zoomed ? " zoomed" : ""}`}
        ref={stageRef}
        onClick={(e) => {
          if (!zoomed && e.target === e.currentTarget) onClose();
        }}
        onDoubleClick={(e) => (zoomed ? reset() : zoomBy(3, { x: e.clientX, y: e.clientY }))}
        onPointerDown={(e) => {
          if (!zoomed) return;
          dragging.current = { x: e.clientX - offset.x, y: e.clientY - offset.y };
          (e.target as HTMLElement).setPointerCapture(e.pointerId);
        }}
        onPointerMove={(e) => {
          if (!dragging.current) return;
          setOffset({
            x: e.clientX - dragging.current.x,
            y: e.clientY - dragging.current.y,
          });
        }}
        onPointerUp={() => (dragging.current = null)}
      >
        {many && !zoomed && (
          <button
            className="lightbox-nav prev"
            onClick={(e) => {
              e.stopPropagation();
              step(-1);
            }}
            aria-label={t("viewer.previous")}
          >
            ‹
          </button>
        )}
        <img
          ref={imgRef}
          src={src}
          alt={photo.name}
          draggable={false}
          decoding="async"
          onLoad={(e) => {
            const el = e.currentTarget;
            setSizes({ layout: el.offsetWidth, natural: el.naturalWidth });
          }}
          style={{
            transform: `translate(${offset.x}px, ${offset.y}px) scale(${scale})`,
          }}
        />
        {many && !zoomed && (
          <button
            className="lightbox-nav next"
            onClick={(e) => {
              e.stopPropagation();
              step(1);
            }}
            aria-label={t("viewer.next")}
          >
            ›
          </button>
        )}
      </div>

      <div className="lightbox-meta mono">
        <span>
          {photo.kind ??
            (photo.width > 0 ? `${photo.width} × ${photo.height}` : "")}
        </span>
        <span>{formatBytes(photo.size, lang)}</span>
        <span>{formatDate(photo.taken, lang)}</span>
        {photo.blur !== null && (
          <span>
            {t("viewer.sharpness")} {Math.round(photo.blur)}
          </span>
        )}
        <span className="hint">{t("viewer.zoomHint")}</span>
      </div>
    </div>
  );
}
