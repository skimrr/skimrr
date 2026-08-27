import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Photo, formatBytes } from "../types";

function CheckIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round">
      <path d="M5 12.5 10 17.5 19 7" />
    </svg>
  );
}

/* Nothing is ever moved sight-unseen: the batch is shown as a grid first, and
   any photo can be pulled back out of it before confirming. */
export function ConfirmModal({
  photos,
  onCancel,
  onConfirm,
}: {
  photos: Photo[];
  onCancel: () => void;
  onConfirm: (paths: string[]) => void;
}) {
  const { t, i18n } = useTranslation();
  const [excluded, setExcluded] = useState<Set<string>>(new Set());

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  const kept = photos.filter((p) => !excluded.has(p.path));
  const bytes = kept.reduce((sum, p) => sum + p.size, 0);

  function toggle(path: string) {
    setExcluded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  return (
    <div className="modal-overlay" onClick={onCancel} role="presentation">
      <div
        className="modal modal-wide"
        role="dialog"
        aria-modal="true"
        aria-label={t("confirm.title", { count: kept.length })}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{t("confirm.title", { count: kept.length })}</h2>
        <p className="modal-sub">
          {t("confirm.body", {
            count: kept.length,
            size: formatBytes(bytes, i18n.language),
          })}
        </p>

        <div className="review-grid">
          {photos.map((photo) => {
            const out = excluded.has(photo.path);
            return (
              <button
                key={photo.path}
                className={`review-cell${out ? " excluded" : ""}`}
                onClick={() => toggle(photo.path)}
                title={photo.path}
                aria-pressed={!out}
              >
                <span className="review-thumb">
                  <img
                    src={convertFileSrc(photo.thumb ?? photo.preview)}
                    alt={photo.name}
                    loading="lazy"
                    decoding="async"
                  />
                  <span className="review-mark">{out ? "↺" : "✕"}</span>
                </span>
                <span className="review-name mono">{photo.name}</span>
                <span className="review-size mono">
                  {formatBytes(photo.size, i18n.language)}
                </span>
              </button>
            );
          })}
        </div>

        <p className="modal-hint">
          {excluded.size > 0
            ? t("confirm.restoreHint", { count: excluded.size })
            : t("confirm.excludeHint")}
        </p>

        <div className="modal-actions">
          <button className="btn-ghost" onClick={onCancel} autoFocus>
            {t("confirm.cancel")}
          </button>
          <button
            className="btn-danger"
            disabled={kept.length === 0}
            onClick={() => onConfirm(kept.map((p) => p.path))}
          >
            {t("confirm.confirm", { count: kept.length })}
          </button>
        </div>
      </div>
    </div>
  );
}

/* Same "nothing happens sight-unseen" review as ConfirmModal, but for the opposite
   direction: these files are about to be copied INTO Photos, not moved to a trash, so
   the action reads as constructive (primary, not danger) even though the review
   pattern — a grid, any item can be pulled back out — is identical. */
export function ImportConfirmModal({
  photos,
  onCancel,
  onConfirm,
}: {
  photos: Photo[];
  onCancel: () => void;
  onConfirm: (paths: string[]) => void;
}) {
  const { t, i18n } = useTranslation();
  const [excluded, setExcluded] = useState<Set<string>>(new Set());

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  const kept = photos.filter((p) => !excluded.has(p.path));

  function toggle(path: string) {
    setExcluded((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  return (
    <div className="modal-overlay" onClick={onCancel} role="presentation">
      <div
        className="modal modal-wide"
        role="dialog"
        aria-modal="true"
        aria-label={t("fusion.importConfirmTitle", { count: kept.length })}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{t("fusion.importConfirmTitle", { count: kept.length })}</h2>
        <p className="modal-sub">{t("fusion.importConfirmBody", { count: kept.length })}</p>

        <div className="review-grid">
          {photos.map((photo) => {
            const out = excluded.has(photo.path);
            return (
              <button
                key={photo.path}
                className={`review-cell${out ? " excluded" : ""}`}
                onClick={() => toggle(photo.path)}
                title={photo.path}
                aria-pressed={!out}
              >
                <span className="review-thumb">
                  <img
                    src={convertFileSrc(photo.thumb ?? photo.preview)}
                    alt={photo.name}
                    loading="lazy"
                    decoding="async"
                  />
                  <span className="review-mark">{out ? "↺" : "✕"}</span>
                </span>
                <span className="review-name mono">{photo.name}</span>
                <span className="review-size mono">
                  {formatBytes(photo.size, i18n.language)}
                </span>
              </button>
            );
          })}
        </div>

        <p className="modal-hint">
          {excluded.size > 0
            ? t("confirm.restoreHint", { count: excluded.size })
            : t("confirm.excludeHint")}
        </p>

        <div className="modal-actions">
          <button className="btn-ghost" onClick={onCancel} autoFocus>
            {t("confirm.cancel")}
          </button>
          <button
            className="btn-primary"
            disabled={kept.length === 0}
            onClick={() => onConfirm(kept.map((p) => p.path))}
          >
            {t("fusion.importConfirmAction", { count: kept.length })}
          </button>
        </div>
      </div>
    </div>
  );
}

export function EmptyTrashModal({
  count,
  size,
  onCancel,
  onConfirm,
}: {
  count: number;
  size: string;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const { t } = useTranslation();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onCancel]);

  return (
    <div className="modal-overlay" onClick={onCancel} role="presentation">
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("trash.emptyTitle")}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{t("trash.emptyTitle")}</h2>
        <p className="modal-sub">{t("trash.emptyBody", { count, size })}</p>
        <div className="modal-actions">
          <button className="btn-ghost" onClick={onCancel} autoFocus>
            {t("confirm.cancel")}
          </button>
          <button className="btn-danger" onClick={onConfirm}>
            {t("trash.emptyConfirm")}
          </button>
        </div>
      </div>
    </div>
  );
}

export function UndoToast({
  count,
  onUndo,
}: {
  count: number;
  onUndo: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="toast" role="status">
      <span>{t("toast.moved", { count })}</span>
      <button className="toast-undo" onClick={onUndo}>
        {t("toast.undo")}
      </button>
    </div>
  );
}

/* A day's photos as a contact sheet first — the same "see the whole batch before
   committing to one" idea as ConfirmModal, but for looking rather than deciding.
   Clicking a thumbnail is what actually opens the full-screen viewer, on top of this
   grid rather than instead of it, so closing the viewer lands back here.

   The small circular pick mark — a second, independent way to choose photos for a
   side-by-side compare, the way the Duplicates tab does for a group — stays out of
   the grid entirely until "Comparer" is clicked once. A plain contact sheet reads
   better than one sprinkled with controls for a feature most visits never use. */
export function DayGridModal({
  title,
  photos,
  onSelect,
  onCompare,
  onClose,
}: {
  title: string;
  photos: Photo[];
  onSelect: (index: number) => void;
  onCompare: (photos: Photo[]) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [compareMode, setCompareMode] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  function toggle(path: string) {
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  }

  return (
    <div className="modal-overlay" onClick={onClose} role="presentation">
      <div
        className="modal modal-wide"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{title}</h2>

        <div className="review-grid">
          {photos.map((photo, i) => {
            const on = picked.has(photo.path);
            return (
              <button
                key={photo.path}
                className="review-cell"
                onClick={() => onSelect(i)}
                title={photo.name}
              >
                <span className="review-thumb">
                  <img
                    src={convertFileSrc(photo.thumb ?? photo.preview)}
                    alt={photo.name}
                    loading="lazy"
                    decoding="async"
                  />
                  {compareMode && (
                    <span
                      role="button"
                      tabIndex={0}
                      className={`review-pick${on ? " on" : ""}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        toggle(photo.path);
                      }}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" || e.key === " ") {
                          e.preventDefault();
                          e.stopPropagation();
                          toggle(photo.path);
                        }
                      }}
                      aria-pressed={on}
                      aria-label={t("days.compareSelect")}
                      title={t("days.compareSelect")}
                    >
                      <CheckIcon />
                    </span>
                  )}
                </span>
                <span className="review-name mono">{photo.name}</span>
              </button>
            );
          })}
        </div>

        {compareMode && (
          <p className="modal-hint">
            {picked.size >= 2
              ? t("days.compareCount", { count: picked.size })
              : t("days.compareHint")}
          </p>
        )}

        <div className="modal-actions">
          <button className="btn-ghost" onClick={onClose} autoFocus>
            {t("actions.close")}
          </button>
          {compareMode && (
            <button
              className="btn-ghost"
              onClick={() => {
                setCompareMode(false);
                setPicked(new Set());
              }}
            >
              {t("confirm.cancel")}
            </button>
          )}
          <button
            className="btn-primary"
            disabled={compareMode && picked.size < 2}
            onClick={() =>
              compareMode
                ? onCompare(photos.filter((p) => picked.has(p.path)))
                : setCompareMode(true)
            }
          >
            {t("compare.open")}
          </button>
        </div>
      </div>
    </div>
  );
}
