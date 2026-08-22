import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { TrashBatch, formatBytes } from "../types";

export function TrashScreen({
  batches,
  onRestore,
  onEmpty,
  onBack,
}: {
  batches: TrashBatch[];
  onRestore: (batchId: string) => void;
  onEmpty: () => void;
  onBack: () => void;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;
  const total = batches.reduce((sum, b) => sum + b.photos.length, 0);
  const bytes = batches.reduce((sum, b) => sum + b.bytes, 0);

  return (
    <main className="results">
      <div className="summary">
        <span className="text">
          {total > 0
            ? t("trash.summary", { count: total, size: formatBytes(bytes, lang) })
            : t("trash.empty")}
        </span>
        <span className="spacer" />
        {total > 0 && (
          <button className="btn-danger" onClick={onEmpty}>
            {t("trash.emptyAction")}
          </button>
        )}
        <button className="btn-ghost" onClick={onBack}>
          {t("trash.back")}
        </button>
      </div>

      {total === 0 ? (
        <div className="empty">
          <h2>{t("trash.empty")}</h2>
          <p>{t("trash.emptyBody2")}</p>
        </div>
      ) : (
        batches.map((batch) => (
          <section className="group-card" key={batch.batch_id}>
            <div className="batch-head">
              <span className="group-label">
                {new Intl.DateTimeFormat(lang, {
                  dateStyle: "medium",
                  timeStyle: "short",
                }).format(new Date(batch.when))}
                {" · "}
                {t("trash.batchCount", { count: batch.photos.length })}
                {" · "}
                {formatBytes(batch.bytes, lang)}
              </span>
              <button
                className="btn-ghost"
                onClick={() => onRestore(batch.batch_id)}
              >
                {t("trash.restore")}
              </button>
            </div>
            <div className="blur-grid">
              {batch.photos.map((photo) => (
                <span
                  className="cell"
                  key={photo.stored_path}
                  title={photo.original}
                >
                  <img
                    src={convertFileSrc(photo.preview)}
                    alt={photo.name}
                    loading="lazy"
                    decoding="async"
                  />
                  <span className="score mono">{photo.name}</span>
                </span>
              ))}
            </div>
          </section>
        ))
      )}
    </main>
  );
}
