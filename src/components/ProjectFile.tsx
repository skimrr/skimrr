import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { formatBytes } from "../types";

export type Mode = "project" | "thumbnails" | "originals";

type Skipped = { library: number; unsafe_name: number; outside_roots: number };
type Estimate = { photos: number; bytes: number; skipped: Skipped };
type Exported = {
  path: string;
  bytes: number;
  photos: number;
  encrypted: boolean;
  skipped: Skipped;
};
type Peek = {
  encrypted: boolean;
  has_thumbnails: boolean;
  has_originals: boolean;
  photos: number;
  bytes: number;
};
type Imported = {
  name: string;
  photos: number;
  resolved: number;
  relocated: number;
  missing: number;
  restored: number;
  kept_existing: number;
  encrypted: boolean;
};

const MODES: Mode[] = ["project", "thumbnails", "originals"];

function ShareIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 14.5V3.5" />
      <path d="M8.5 7 12 3.5 15.5 7" />
      <path d="M4.5 13.5v4a2 2 0 0 0 2 2h11a2 2 0 0 0 2-2v-4" />
    </svg>
  );
}

function FileIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false" fill="none" stroke="currentColor" strokeWidth="1.9" strokeLinecap="round" strokeLinejoin="round">
      <path d="M13.5 3.5H7a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V9z" />
      <path d="M13.5 3.5V9H19" />
    </svg>
  );
}

/* One place for everything that leaves Skimrr.
 *
 * The two used to sit apart and unlabelled-alike: an icon-only share button buried in
 * the Overview tab, and an export link floating in the top bar. Nothing said they were
 * two answers to the same question — what comes out of here? — so each made the other
 * harder to find. Putting the question first and the two answers under it also gives the
 * editor hand-off the thing it most needed: a count, stated before anything opens.
 * Sending five thousand files to Lightroom should not be one click away. */
export function SendModal({
  keptCount,
  onEditor,
  onExport,
  onClose,
}: {
  keptCount: number;
  onEditor: () => void;
  onExport: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation();
  useEscape(onClose);

  return (
    <div className="modal-overlay" onClick={onClose} role="presentation">
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("project.send.title")}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{t("project.send.title")}</h2>
        <p className="modal-sub">{t("project.send.body")}</p>

        <div className="project-modes">
          {keptCount > 0 && (
            <button className="project-mode project-mode-row" onClick={onEditor}>
              <span className="project-mode-icon">
                <ShareIcon />
              </span>
              <span>
                <span className="project-mode-name">{t("project.send.editor.name")}</span>
                <span className="project-mode-note">
                  {t("project.send.editor.note", { count: keptCount })}
                </span>
              </span>
            </button>
          )}
          <button className="project-mode project-mode-row" onClick={onExport}>
            <span className="project-mode-icon">
              <FileIcon />
            </span>
            <span>
              <span className="project-mode-name">{t("project.send.file.name")}</span>
              <span className="project-mode-note">{t("project.send.file.note")}</span>
            </span>
          </button>
        </div>

        <div className="modal-actions">
          <button className="btn-quiet" onClick={onClose}>
            {t("project.cancel")}
          </button>
        </div>
      </div>
    </div>
  );
}

const FILTER = [{ name: "Skimrr project", extensions: ["skimrr"] }];

function useEscape(onClose: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
}

/* A password lives in React state for as long as the dialog is open and nowhere else.
   It is never written to localStorage, never sent anywhere, and the field is cleared
   when the dialog closes — the backend wipes its own copy the moment it has derived a
   key from it. */
function PasswordField({
  value,
  onChange,
  label,
  hint,
  autoFocus,
}: {
  value: string;
  onChange: (v: string) => void;
  label: string;
  hint?: string;
  autoFocus?: boolean;
}) {
  const [shown, setShown] = useState(false);
  return (
    <label className="project-field">
      <span className="project-field-label">{label}</span>
      <span className="project-password">
        <input
          type={shown ? "text" : "password"}
          value={value}
          autoFocus={autoFocus}
          autoComplete="off"
          spellCheck={false}
          onChange={(e) => onChange(e.target.value)}
        />
        <button
          type="button"
          className="btn-quiet project-reveal"
          onClick={() => setShown((s) => !s)}
        >
          {shown ? "•••" : "abc"}
        </button>
      </span>
      {hint && <span className="project-hint">{hint}</span>}
    </label>
  );
}

export function ExportModal({
  threshold,
  suggestedName,
  onClose,
}: {
  threshold: number;
  suggestedName: string;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation();
  const [mode, setMode] = useState<Mode>("project");
  const [encrypt, setEncrypt] = useState(false);
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [estimate, setEstimate] = useState<Estimate | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<Exported | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEscape(onClose);

  useEffect(() => {
    let live = true;
    invoke<Estimate>("export_estimate", { mode })
      .then((e) => live && setEstimate(e))
      .catch(() => live && setEstimate(null));
    return () => {
      live = false;
    };
  }, [mode]);

  const mismatch = encrypt && confirmPassword.length > 0 && password !== confirmPassword;
  const ready = !encrypt || (password.length > 0 && password === confirmPassword);

  async function run() {
    setError(null);
    const dest = await save({ defaultPath: `${suggestedName}.skimrr`, filters: FILTER });
    if (!dest) return;
    setBusy(true);
    try {
      const result = await invoke<Exported>("export_project", {
        dest,
        mode,
        threshold,
        name: suggestedName,
        password: encrypt ? password : null,
      });
      setDone(result);
    } catch (e) {
      setError(String(e));
    } finally {
      // Whatever happened, the copy in this component goes now.
      setBusy(false);
      setPassword("");
      setConfirmPassword("");
    }
  }

  return (
    <div className="modal-overlay" onClick={onClose} role="presentation">
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("project.export.title")}
        onClick={(e) => e.stopPropagation()}
      >
        {done ? (
          <>
            <h2>{t("project.export.doneTitle")}</h2>
            <p className="modal-sub">
              {t("project.export.doneBody", {
                count: done.photos,
                size: formatBytes(done.bytes, i18n.language),
              })}
            </p>
            {done.encrypted && (
              <p className="modal-sub">{t("project.export.doneEncrypted")}</p>
            )}
            {done.skipped.library > 0 && (
              <p className="modal-sub modal-warn">
                {t("project.export.skippedLibrary", { count: done.skipped.library })}
              </p>
            )}
            <div className="modal-actions">
              <button className="btn-primary" onClick={onClose}>
                {t("project.close")}
              </button>
            </div>
          </>
        ) : (
          <>
            <h2>{t("project.export.title")}</h2>
            <p className="modal-sub">{t("project.export.body")}</p>

            <div className="project-modes">
              {MODES.map((m) => (
                <button
                  key={m}
                  className={`project-mode${mode === m ? " on" : ""}`}
                  onClick={() => setMode(m)}
                  aria-pressed={mode === m}
                >
                  <span className="project-mode-name">{t(`project.mode.${m}.name`)}</span>
                  <span className="project-mode-note">{t(`project.mode.${m}.note`)}</span>
                </button>
              ))}
            </div>

            {estimate && (
              <p className="modal-sub mono">
                {t("project.export.estimate", {
                  count: estimate.photos,
                  size: formatBytes(estimate.bytes, i18n.language),
                })}
              </p>
            )}
            {estimate && estimate.skipped.library > 0 && (
              <p className="modal-sub modal-warn">
                {t("project.export.skippedLibrary", { count: estimate.skipped.library })}
              </p>
            )}

            <label className="project-check">
              <input
                type="checkbox"
                checked={encrypt}
                onChange={(e) => {
                  setEncrypt(e.target.checked);
                  setPassword("");
                  setConfirmPassword("");
                }}
              />
              <span>{t("project.export.encrypt")}</span>
            </label>

            {encrypt && (
              <>
                <PasswordField
                  label={t("project.password")}
                  value={password}
                  onChange={setPassword}
                  autoFocus
                  hint={t("project.export.passwordHint")}
                />
                <PasswordField
                  label={t("project.export.passwordAgain")}
                  value={confirmPassword}
                  onChange={setConfirmPassword}
                />
                {mismatch && <p className="error">{t("project.export.mismatch")}</p>}
              </>
            )}

            {/* Le message du backend, pas un message générique. Il sait pourquoi — « aucune
                de ces photos ne peut être exportée », « impossible d'écrire le fichier :
                permission refusée » — et l'écraser par « le fichier n'a pas pu être
                écrit » retire à la personne la seule information qui lui permettrait d'agir.
                Ces messages sont écrits pour être lus, pas pour être avalés. */}
            {error && (
              <p className="error">
                {t("project.export.failed")}
                <span className="error-detail">{error}</span>
              </p>
            )}

            <div className="modal-actions">
              <button className="btn-quiet" onClick={onClose}>
                {t("project.cancel")}
              </button>
              <button className="btn-primary" onClick={run} disabled={busy || !ready}>
                {busy ? t("project.export.working") : t("project.export.go")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

export function OpenProjectModal({
  path,
  onClose,
  onImported,
}: {
  path: string;
  onClose: () => void;
  onImported: (result: Imported) => void;
}) {
  const { t } = useTranslation();
  const [peek, setPeek] = useState<Peek | null>(null);
  const [password, setPassword] = useState("");
  const [searchRoot, setSearchRoot] = useState<string | null>(null);
  const [restoreRoot, setRestoreRoot] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<Imported | null>(null);
  const [error, setError] = useState<"peek" | "password" | "failed" | null>(null);

  useEscape(onClose);

  useEffect(() => {
    let live = true;
    invoke<Peek>("peek_project", { path })
      .then((p) => live && setPeek(p))
      .catch(() => live && setError("peek"));
    return () => {
      live = false;
    };
  }, [path]);

  const pickFolder = useCallback(async (set: (v: string | null) => void) => {
    const folder = await open({ directory: true, multiple: false });
    if (typeof folder === "string") set(folder);
  }, []);

  async function run() {
    if (!peek) return;
    setError(null);
    setBusy(true);
    try {
      const result = await invoke<Imported>("import_project", {
        path,
        password: peek.encrypted ? password : null,
        searchRoot,
        restoreRoot,
      });
      setPassword("");
      setDone(result);
    } catch (e) {
      /* The backend cannot tell a wrong password from an altered file and does not
         pretend to, so neither does this: one message covering both is the honest one. */
      setError(String(e).includes("password") ? "password" : "failed");
      setBusy(false);
    }
  }

  const name = path.split(/[\\/]/).pop() ?? path;

  return (
    <div className="modal-overlay" onClick={onClose} role="presentation">
      <div
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-label={t("project.open.title")}
        onClick={(e) => e.stopPropagation()}
      >
        <h2>{t("project.open.title")}</h2>
        <p className="modal-sub mono">{name}</p>

        {done ? (
          <>
            <p className="modal-sub">
              {t("project.open.doneBody", { name: done.name, count: done.photos })}
            </p>
            {done.restored > 0 && (
              <p className="modal-sub">
                {t("project.open.restored", { count: done.restored })}
              </p>
            )}
            {done.kept_existing > 0 && (
              <p className="modal-sub">
                {t("project.open.keptExisting", { count: done.kept_existing })}
              </p>
            )}
            {done.relocated > 0 && (
              <p className="modal-sub">
                {t("project.open.relocated", { count: done.relocated })}
              </p>
            )}
            {done.missing > 0 && (
              <p className="modal-sub modal-warn">
                {t("project.open.missing", { count: done.missing })}
              </p>
            )}
            <div className="modal-actions">
              <button className="btn-primary" onClick={() => onImported(done)}>
                {t("project.close")}
              </button>
            </div>
          </>
        ) : error === "peek" ? (
          <>
            <p className="error">{t("project.open.notAProject")}</p>
            <div className="modal-actions">
              <button className="btn-primary" onClick={onClose}>
                {t("project.close")}
              </button>
            </div>
          </>
        ) : (
          <>
            {peek && (
              <p className="modal-sub">
                {peek.has_originals
                  ? t("project.open.withOriginals")
                  : peek.has_thumbnails
                    ? t("project.open.withThumbnails")
                    : t("project.open.findingsOnly")}
              </p>
            )}

            {peek?.encrypted && (
              <PasswordField
                label={t("project.password")}
                value={password}
                onChange={setPassword}
                autoFocus
                hint={t("project.open.passwordHint")}
              />
            )}

            {peek && !peek.has_originals && (
              <div className="project-field">
                <span className="project-field-label">{t("project.open.whereAre")}</span>
                <button className="btn-quiet" onClick={() => pickFolder(setSearchRoot)}>
                  {searchRoot ?? t("project.open.chooseFolder")}
                </button>
                <span className="project-hint">{t("project.open.whereAreHint")}</span>
              </div>
            )}

            {peek?.has_originals && (
              <div className="project-field">
                <span className="project-field-label">{t("project.open.whereTo")}</span>
                <button className="btn-quiet" onClick={() => pickFolder(setRestoreRoot)}>
                  {restoreRoot ?? t("project.open.chooseFolder")}
                </button>
                <span className="project-hint">{t("project.open.whereToHint")}</span>
              </div>
            )}

            {error === "password" && <p className="error">{t("project.open.wrongPassword")}</p>}
            {error === "failed" && <p className="error">{t("project.open.failed")}</p>}

            <div className="modal-actions">
              <button className="btn-quiet" onClick={onClose}>
                {t("project.cancel")}
              </button>
              <button
                className="btn-primary"
                onClick={run}
                disabled={busy || !peek || (peek.encrypted && password.length === 0)}
              >
                {busy ? t("project.open.working") : t("project.open.go")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

/// Opens the system picker and returns the chosen `.skimrr`, or nothing.
export async function chooseProjectFile(): Promise<string | null> {
  const picked = await open({ multiple: false, filters: FILTER });
  return typeof picked === "string" ? picked : null;
}

export type { Imported };
