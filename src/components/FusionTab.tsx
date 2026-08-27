import { useMemo } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { Photo, PhotosComparison, View, formatBytes } from "../types";

/**
 * "Retour de vacances": a Source folder (an SD card, a rushes folder) has already been
 * scanned like any other — this tab is only step 2, checking what Photos (the
 * Destination) already has, so nothing gets imported twice and nothing already safe
 * on this Mac lingers on the card. The comparison itself is metadata-only (filename +
 * exact original byte size, see `fusion::already_in_photos` on the Rust side): most of
 * a real Photos library with iCloud "Optimize Mac Storage" on is not fully downloaded,
 * so there is no second perceptual hash to lean on here.
 */
export function FusionTab({
  view,
  libraryPath,
  running,
  error,
  result,
  onPickLibrary,
  onCompare,
  onRequestImport,
  onRequestClean,
  onExpand,
}: {
  view: View;
  libraryPath: string | null;
  running: boolean;
  error: "unreadable" | null;
  result: PhotosComparison | null;
  onPickLibrary: () => void;
  onCompare: () => void;
  onRequestImport: (photos: Photo[]) => void;
  onRequestClean: (photos: Photo[]) => void;
  onExpand: (indices: number[], at: number) => void;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;

  const byPath = useMemo(() => {
    const map = new Map<string, number>();
    view.photos.forEach((p, i) => map.set(p.path, i));
    return map;
  }, [view.photos]);

  const already = useMemo(
    () => (result?.already_in_photos ?? []).map((p) => byPath.get(p)).filter((i): i is number => i !== undefined),
    [result, byPath],
  );
  const missing = useMemo(
    () => (result?.missing_from_photos ?? []).map((p) => byPath.get(p)).filter((i): i is number => i !== undefined),
    [result, byPath],
  );
  const alreadyBytes = already.reduce((sum, i) => sum + view.photos[i].size, 0);

  return (
    <div className="fusion">
      <div className="fusion-bar">
        <div className="fusion-library">
          <span className="fusion-library-label">{t("fusion.library")}</span>
          <span className="fusion-library-path mono" title={libraryPath ?? undefined}>
            {libraryPath ? libraryPath.split("/").pop() : t("fusion.noLibrary")}
          </span>
        </div>
        <button className="btn-ghost" onClick={onPickLibrary} disabled={running}>
          {t("fusion.changeLibrary")}
        </button>
        <button className="btn-primary" onClick={onCompare} disabled={running || !libraryPath}>
          {running ? t("fusion.comparing") : t("fusion.compare")}
        </button>
      </div>

      {error === "unreadable" && (
        <p className="notice fusion-error">{t("fusion.unreadable")}</p>
      )}

      {!result && !running && error !== "unreadable" && (
        <p className="fusion-intro">{t("fusion.intro")}</p>
      )}

      {result && (
        <>
          <div className="kpis">
            <div className="kpi">
              <span className="kpi-label">{t("fusion.alreadyKpi")}</span>
              <span className="kpi-value">{already.length.toLocaleString(lang)}</span>
            </div>
            <div className="kpi">
              <span className="kpi-label">{t("fusion.missingKpi")}</span>
              <span className="kpi-value">{missing.length.toLocaleString(lang)}</span>
            </div>
            <div className="kpi">
              <span className="kpi-label">{t("fusion.reclaimKpi")}</span>
              <span className="kpi-value">{formatBytes(alreadyBytes, lang)}</span>
            </div>
          </div>

          <section className="panel fusion-panel">
            <div className="fusion-panel-head">
              <h2>{t("fusion.alreadyTitle")}</h2>
              {already.length > 0 && (
                <button
                  className="btn-ghost"
                  onClick={() => onRequestClean(already.map((i) => view.photos[i]))}
                >
                  {t("fusion.cleanAction", { count: already.length })}
                </button>
              )}
            </div>
            {already.length === 0 ? (
              <p className="fusion-empty">{t("fusion.alreadyNone")}</p>
            ) : (
              <ul className="review-grid">
                {already.map((i) => {
                  const photo = view.photos[i];
                  return (
                    <li key={photo.path}>
                      <button
                        className="review-cell"
                        onClick={() => onExpand(already, already.indexOf(i))}
                        title={photo.path}
                      >
                        <span className="review-thumb">
                          <img src={convertFileSrc(photo.preview)} alt={photo.name} loading="lazy" decoding="async" />
                        </span>
                        <span className="review-name mono">{photo.name}</span>
                        <span className="review-size mono">{formatBytes(photo.size, lang)}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>

          <section className="panel fusion-panel">
            <div className="fusion-panel-head">
              <h2>{t("fusion.missingTitle")}</h2>
              {missing.length > 0 && (
                <button
                  className="btn-primary"
                  onClick={() => onRequestImport(missing.map((i) => view.photos[i]))}
                >
                  {t("fusion.importAction", { count: missing.length })}
                </button>
              )}
            </div>
            {missing.length === 0 ? (
              <p className="fusion-empty">{t("fusion.missingNone")}</p>
            ) : (
              <ul className="review-grid">
                {missing.map((i) => {
                  const photo = view.photos[i];
                  return (
                    <li key={photo.path}>
                      <button
                        className="review-cell"
                        onClick={() => onExpand(missing, missing.indexOf(i))}
                        title={photo.path}
                      >
                        <span className="review-thumb">
                          <img src={convertFileSrc(photo.preview)} alt={photo.name} loading="lazy" decoding="async" />
                        </span>
                        <span className="review-name mono">{photo.name}</span>
                        <span className="review-size mono">{formatBytes(photo.size, lang)}</span>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        </>
      )}
    </div>
  );
}
