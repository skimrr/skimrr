import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import { Licence } from "../types";

const BUY_URL = "https://skimrr.com";

/* Reached only when someone tries to move files: scanning and reviewing are free, so
   a visitor sees their own duplicates before being asked for anything. */
export function Activation({
  onActivated,
  onClose,
}: {
  onActivated: (licence: Licence) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, busy]);

  async function submit() {
    if (!key.trim() || busy) return;
    setBusy(true);
    setError(null);
    try {
      const licence = await invoke<Licence>("activate_licence", { key });
      if (licence.activated) {
        onActivated(licence);
      } else {
        setError(messageFor(licence.message));
      }
    } catch {
      setError(t("licence.errors.unexpected"));
    } finally {
      setBusy(false);
    }
  }

  /* The Worker forwards Lemon Squeezy's own wording for anything we have not named,
     which is more useful than a generic failure. */
  function messageFor(code: string | null): string {
    if (!code) return t("licence.errors.invalid");
    if (code === "offline") return t("licence.errors.offline");
    if (code === "empty_key") return t("licence.errors.invalid");
    if (/activation limit/i.test(code)) return t("licence.errors.limit");
    return code;
  }

  return (
    <div className="modal-overlay" onClick={() => !busy && onClose()} role="presentation">
      <div
        className="modal activation"
        role="dialog"
        aria-modal="true"
        aria-label={t("licence.title")}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{t("licence.title")}</h2>
        <p className="modal-sub">{t("licence.body")}</p>

        <label className="licence-field">
          <span className="licence-label">{t("licence.field")}</span>
          <input
            className="mono"
            value={key}
            onChange={(e) => setKey(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && submit()}
            placeholder="XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX"
            spellCheck={false}
            autoFocus
            disabled={busy}
          />
        </label>

        {error && <p className="licence-error">{error}</p>}

        <div className="modal-actions">
          <button
            className="btn-ghost"
            onClick={() => openUrl(BUY_URL).catch(() => undefined)}
            disabled={busy}
          >
            {t("licence.buy")}
          </button>
          <span className="spacer" />
          <button className="btn-ghost" onClick={onClose} disabled={busy}>
            {t("confirm.cancel")}
          </button>
          <button className="btn-primary" onClick={submit} disabled={busy || !key.trim()}>
            {busy ? t("licence.checking") : t("licence.activate")}
          </button>
        </div>

        <p className="licence-note">{t("licence.note")}</p>
      </div>
    </div>
  );
}
