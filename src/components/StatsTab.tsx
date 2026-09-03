import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { View, formatBytes } from "../types";
import { WorldMap } from "./WorldMap";

/** Slice of a part-to-whole reading, in the order it is drawn and listed. */
interface Slice {
  key: string;
  label: string;
  count: number;
  color: string;
}

/**
 * A donut is only honest for part-to-whole at a glance, with few segments — three
 * here. Formats and years are bars instead: those are comparisons, and comparing
 * angles is what a pie chart is worst at.
 */
function Donut({ slices, total }: { slices: Slice[]; total: number }) {
  const R = 52;
  const STROKE = 16;
  const C = 2 * Math.PI * R;
  // A 2px gap in the surface colour separates touching segments.
  const GAP = 2;

  let offset = 0;
  const arcs = slices
    .filter((s) => s.count > 0)
    .map((s) => {
      const length = (s.count / total) * C;
      const arc = { ...s, dash: Math.max(0, length - GAP), offset };
      offset += length;
      return arc;
    });

  return (
    <svg viewBox="0 0 140 140" className="donut" role="img">
      <circle cx="70" cy="70" r={R} className="donut-track" strokeWidth={STROKE} />
      {arcs.map((a) => (
        <circle
          key={a.key}
          cx="70"
          cy="70"
          r={R}
          fill="none"
          stroke={a.color}
          strokeWidth={STROKE}
          strokeDasharray={`${a.dash} ${C - a.dash}`}
          strokeDashoffset={-a.offset}
          transform="rotate(-90 70 70)"
        />
      ))}
    </svg>
  );
}

function Bars({ rows }: { rows: { label: string; value: number }[] }) {
  const max = Math.max(1, ...rows.map((r) => r.value));
  return (
    <ul className="bars">
      {rows.map((r) => (
        <li key={r.label}>
          <span className="bar-label mono">{r.label}</span>
          <span className="bar-track">
            <span className="bar-fill" style={{ width: `${(r.value / max) * 100}%` }} />
          </span>
          <span className="bar-value mono">{r.value}</span>
        </li>
      ))}
    </ul>
  );
}

/* The share glyph: the same open tray `DropIcon` uses for an incoming drop, with the
   arrow reversed. The two are opposite motions — files arriving, files leaving — so
   drawing them from one shape family is what makes the pair legible. */
/** The photographs a clean-up would leave standing: not a surplus copy of anything, and
    not under the blur cut. Exported because the send dialog offers exactly this set to
    an editor, and a second definition of "kept" would eventually disagree with the donut
    that sits under it. */
export function keptPaths(view: View, blurThreshold: number): string[] {
  const inGroups = new Set<number>();
  for (const group of view.groups) {
    // The keeper is not surplus: only the rest of a group could go.
    group.indices.forEach((idx, pos) => {
      if (pos !== group.suggested) inGroups.add(idx);
    });
  }
  return view.photos
    .filter((p, i) => !inGroups.has(i) && !(p.blur !== null && p.blur < blurThreshold))
    .map((p) => p.path);
}

export function StatsTab({
  view,
  blurThreshold,
}: {
  view: View;
  blurThreshold: number;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.language;

  const data = useMemo(() => {
    const inGroups = new Set<number>();
    for (const group of view.groups) {
      // The keeper is not surplus: only the rest of a group could go.
      group.indices.forEach((idx, pos) => {
        if (pos !== group.suggested) inGroups.add(idx);
      });
    }
    const blurry = view.photos.reduce(
      (n, p, i) =>
        !inGroups.has(i) && p.blur !== null && p.blur < blurThreshold ? n + 1 : n,
      0,
    );
    const total = view.photos.length;


    const byFormat = new Map<string, number>();
    const byDevice = new Map<string, number>();
    const byYear = new Map<string, number>();
    for (const photo of view.photos) {
      // The format comes from its own field, never from the display name: a library
      // photo is named after the camera that took it, and cutting that at the last dot
      // used to file "iPhone 13" alongside JPG and HEIC.
      const format = photo.format || "?";
      byFormat.set(format, (byFormat.get(format) ?? 0) + 1);
      if (photo.device) {
        byDevice.set(photo.device, (byDevice.get(photo.device) ?? 0) + 1);
      }
      if (photo.taken) {
        const year = String(new Date(photo.taken * 1000).getFullYear());
        byYear.set(year, (byYear.get(year) ?? 0) + 1);
      }
    }

    /* Past a handful of classes adjacent bars stop being readable, so the tail is
       folded into one row rather than drawn as ever-thinner slivers. */
    const topFew = (counts: Map<string, number>) => {
      const rows = [...counts.entries()].sort((a, b) => b[1] - a[1]);
      const head = rows.slice(0, 5);
      const tail = rows.slice(5).reduce((n, [, v]) => n + v, 0);
      if (tail > 0) head.push([t("stats.other"), tail]);
      return head.map(([label, value]) => ({ label, value }));
    };

    return {
      total,
      dupes: inGroups.size,
      blurry,
      rest: Math.max(0, total - inGroups.size - blurry),
      formats: topFew(byFormat),
      devices: topFew(byDevice),
      years: [...byYear.entries()]
        .sort((a, b) => a[0].localeCompare(b[0]))
        .map(([label, value]) => ({ label, value })),
    };
  }, [view, blurThreshold, t]);

  const slices: Slice[] = [
    { key: "dupes", label: t("stats.dupes"), count: data.dupes, color: "var(--series-1)" },
    { key: "blurry", label: t("stats.blurry"), count: data.blurry, color: "var(--series-2)" },
    { key: "rest", label: t("stats.rest"), count: data.rest, color: "var(--series-rest)" },
  ];

  return (
    <div className="stats">
      <div className="kpis">
        <div className="kpi">
          <span className="kpi-label">{t("stats.photos")}</span>
          <span className="kpi-value">{data.total.toLocaleString(lang)}</span>
        </div>
        <div className="kpi">
          <span className="kpi-label">{t("stats.reclaim")}</span>
          <span className="kpi-value">{formatBytes(view.reclaimable_bytes, lang)}</span>
        </div>
        <div className="kpi">
          <span className="kpi-label">{t("stats.groups")}</span>
          <span className="kpi-value">{view.groups.length.toLocaleString(lang)}</span>
        </div>
      </div>

      <section className="panel">
        <div className="stats-head">
          <h2>{t("stats.make")}</h2>

        </div>
        <div className="donut-row">
          <Donut slices={slices} total={Math.max(1, data.total)} />
          {/* Identity never rests on colour alone: every slice is named and counted. */}
          <ul className="legend">
            {slices.map((s) => (
              <li key={s.key}>
                <span className="swatch" style={{ background: s.color }} />
                <span className="legend-label">{s.label}</span>
                <span className="legend-value mono">
                  {s.count.toLocaleString(lang)}
                </span>
              </li>
            ))}
          </ul>
        </div>
      </section>

      <section className="panel">
        <h2>{t("stats.map")}</h2>
        <WorldMap photos={view.photos} />
      </section>

      <div className="panel-row">
        <section className="panel">
          <h2>{t("stats.formats")}</h2>
          <Bars rows={data.formats} />
        </section>
        {data.devices.length > 0 && (
          <section className="panel">
            <h2>{t("stats.devices")}</h2>
            <Bars rows={data.devices} />
          </section>
        )}
        {data.years.length > 0 && (
          <section className="panel">
            <h2>{t("stats.years")}</h2>
            <Bars rows={data.years} />
          </section>
        )}
      </div>
    </div>
  );
}
