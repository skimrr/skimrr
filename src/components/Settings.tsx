import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useTranslation } from "react-i18next";
import { LANGUAGES } from "../i18n";
import { Licence, Theme, formatBytes } from "../types";

interface CacheUsage {
  previews_bytes: number;
  previews_files: number;
  scans_bytes: number;
  scans_files: number;
}

const THEMES: Theme[] = ["auto", "light", "dark"];

/**
 * Everything that is a setting, and nothing that is a judgement.
 *
 * Similarity and blur stay in their tabs on purpose: they are read against the results
 * they produce, and a slider in a panel like this one is a knob turned blind. What
 * lands here is what had nowhere else to live: the licence and the devices it is spent
 * on, what the caches weigh, and which version is running.
 */
export function Settings({
  licence,
  onLicence,
  theme,
  onTheme,
  onGuide,
  scanLoaded,
  onCleared,
  onClose,
}: {
  licence: Licence | null;
  onLicence: (licence: Licence) => void;
  theme: Theme;
  onTheme: (theme: Theme) => void;
  onGuide: () => void;
  /** Results on screen are built from these files, so clearing has to close them. */
  scanLoaded: boolean;
  /** Called after a clear that invalidated the results being shown. */
  onCleared: () => void;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;

  const [cache, setCache] = useState<CacheUsage | null>(null);
  const [version, setVersion] = useState("");
  const [busy, setBusy] = useState<"cache" | "licence" | null>(null);
  const [freed, setFreed] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<CacheUsage>("cache_usage").then(setCache).catch(() => undefined);
    invoke<string>("app_version").then(setVersion).catch(() => undefined);
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, busy]);

  const cacheBytes = cache ? cache.previews_bytes + cache.scans_bytes : 0;
  const cacheFiles = cache ? cache.previews_files + cache.scans_files : 0;

  async function clearCache() {
    setBusy("cache");
    setError(null);
    try {
      const gone = await invoke<CacheUsage>("clear_cache");
      setFreed(gone.previews_bytes + gone.scans_bytes);
      setCache(await invoke<CacheUsage>("cache_usage"));
      if (scanLoaded) onCleared();
    } catch {
      setError(t("settings.cache.failed"));
    } finally {
      setBusy(null);
    }
  }

  async function release() {
    setBusy("licence");
    setError(null);
    try {
      onLicence(await invoke<Licence>("deactivate_licence"));
    } catch {
      setError(t("settings.licence.failed"));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="modal-overlay" onClick={() => !busy && onClose()} role="presentation">
      <div
        className="modal settings"
        role="dialog"
        aria-modal="true"
        aria-label={t("settings.title")}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{t("settings.title")}</h2>

        <section className="set-block">
          <h3>{t("settings.licence.title")}</h3>
          {licence?.activated ? (
            <>
              <div className="set-figure">
                <span className="set-badge">{t("settings.licence.active")}</span>
                <span className="set-value mono">
                  {t("settings.licence.devices", {
                    used: licence.activation_usage,
                    limit: licence.activation_limit,
                  })}
                </span>
                <span className="spacer" />
                <button className="btn-ghost" onClick={release} disabled={busy !== null}>
                  {busy === "licence" ? t("settings.working") : t("settings.licence.release")}
                </button>
              </div>
              <p className="set-note">{t("settings.licence.releaseNote")}</p>
            </>
          ) : (
            <p className="set-note">{t("settings.licence.none")}</p>
          )}
        </section>

        <section className="set-block">
          <h3>{t("settings.cache.title")}</h3>
          <div className="set-figure">
            <span className="set-value mono">{formatBytes(cacheBytes, lang)}</span>
            <span className="set-sub mono">
              {t("settings.cache.files", { count: cacheFiles })}
            </span>
            <span className="spacer" />
            <button
              className="btn-ghost"
              onClick={clearCache}
              disabled={busy !== null || cacheFiles === 0}
            >
              {busy === "cache" ? t("settings.working") : t("settings.cache.clear")}
            </button>
          </div>
          <p className="set-note">
            {scanLoaded ? t("settings.cache.closes") : t("settings.cache.note")}
          </p>
          {freed !== null && (
            <p className="set-ok">
              {t("settings.cache.freed", { size: formatBytes(freed, lang) })}
            </p>
          )}
        </section>

        <section className="set-block">
          <h3>{t("settings.look.title")}</h3>
          <label className="set-row">
            <span>{t("language")}</span>
            <select
              className="set-select"
              value={LANGUAGES.some((l) => l.code === lang) ? lang : "en"}
              onChange={(e) => i18n.changeLanguage(e.target.value)}
            >
              {LANGUAGES.map((l) => (
                <option key={l.code} value={l.code}>
                  {l.label}
                </option>
              ))}
            </select>
          </label>
          {/* Trois valeurs, toutes visibles : un menu déroulant natif pour trois choix
              coûte un clic de plus et jure avec le reste de l'interface sur macOS. */}
          <div className="set-row">
            <span>{t("settings.look.theme")}</span>
            <div className="set-chips" role="group" aria-label={t("settings.look.theme")}>
              {THEMES.map((option) => (
                <button
                  key={option}
                  className={`chip${theme === option ? " on" : ""}`}
                  aria-pressed={theme === option}
                  onClick={() => onTheme(option)}
                >
                  {t(`settings.look.themes.${option}`)}
                </button>
              ))}
            </div>
          </div>
        </section>

        <section className="set-block">
          <h3>{t("settings.about.title")}</h3>
          <p className="set-figure">
            <span className="set-value">Skimrr</span>
            {version && <span className="set-sub mono">{version}</span>}
            {licence?.update_available && (
              <span className="set-badge">
                {t("update.available", { version: licence.update_available })}
              </span>
            )}
          </p>
          <div className="set-links">
            <button className="btn-quiet" onClick={onGuide}>
              {t("guide.open")}
            </button>
            <button
              className="btn-quiet"
              onClick={() => openUrl("https://skimrr.com").catch(() => undefined)}
            >
              skimrr.com
            </button>
          </div>
        </section>

        {error && <p className="error">{error}</p>}

        <div className="modal-actions">
          <button className="btn-primary" onClick={onClose} disabled={busy !== null}>
            {t("actions.close")}
          </button>
        </div>
      </div>
    </div>
  );
}
