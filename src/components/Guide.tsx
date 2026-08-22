import { useEffect } from "react";
import { useTranslation } from "react-i18next";

/* Shown once, on the very first launch, and reachable afterwards from the "?" in the
   top bar. Three points and nothing else: what it looks for, that it stays on this
   machine, and that nothing disappears without being seen first. */
export function Guide({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const steps = ["scan", "review", "trash"] as const;

  return (
    <div className="modal-overlay" onClick={onClose} role="presentation">
      <div
        className="modal guide"
        role="dialog"
        aria-modal="true"
        aria-label={t("guide.title")}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{t("guide.title")}</h2>
        <p className="modal-sub">{t("guide.intro")}</p>

        <ol className="guide-steps">
          {steps.map((step, i) => (
            <li key={step}>
              <span className="guide-num mono">{i + 1}</span>
              <span className="guide-body">
                <strong>{t(`guide.${step}.title`)}</strong>
                <span>{t(`guide.${step}.body`)}</span>
              </span>
            </li>
          ))}
        </ol>

        <p className="guide-privacy">{t("guide.privacy")}</p>

        <div className="modal-actions">
          <button className="btn-primary" onClick={onClose} autoFocus>
            {t("guide.start")}
          </button>
        </div>
      </div>
    </div>
  );
}
