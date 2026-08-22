import { convertFileSrc } from "@tauri-apps/api/core";
import { useTranslation } from "react-i18next";
import { formatBytes } from "../types";

export interface Day {
  key: string;
  count: number;
  bytes: number;
  covers: string[];
}

/**
 * The folder as its owner remembers it: by the day the photographs were taken.
 *
 * Selecting days narrows the duplicate and blur tabs to them, which is what makes a
 * folder of several thousand photographs workable. A trip is lived one day at a time
 * and is far easier to judge the same way.
 */
export function DaysTab({
  days,
  selected,
  onToggle,
  onClear,
}: {
  days: Day[];
  selected: Set<string>;
  onToggle: (key: string) => void;
  onClear: () => void;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;

  const label = (key: string) =>
    key
      ? new Intl.DateTimeFormat(lang, { dateStyle: "full" }).format(new Date(`${key}T12:00:00`))
      : t("days.undated");

  return (
    <div className="days">
      <div className="days-bar">
        <span className="days-hint">
          {selected.size > 0
            ? t("days.selected", { count: selected.size })
            : t("days.hint")}
        </span>
        <span className="spacer" />
        {selected.size > 0 && (
          <button className="btn-ghost" onClick={onClear}>
            {t("days.clear")}
          </button>
        )}
      </div>

      <ul className="day-list">
        {days.map((day) => (
          <li key={day.key}>
            <button
              className={`day${selected.has(day.key) ? " on" : ""}`}
              onClick={() => onToggle(day.key)}
              aria-pressed={selected.has(day.key)}
            >
              <span className="day-covers">
                {day.covers.map((cover) => (
                  <img key={cover} src={convertFileSrc(cover)} alt="" loading="lazy" />
                ))}
              </span>
              <span className="day-meta">
                <span className="day-label">{label(day.key)}</span>
                <span className="day-facts mono">
                  {day.count} {t("days.photos")} · {formatBytes(day.bytes, lang)}
                </span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
