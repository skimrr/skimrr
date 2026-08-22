import { useEffect, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Photo, formatBytes } from "../types";

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
                    src={convertFileSrc(photo.preview)}
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
