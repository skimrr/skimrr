import { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { Photo } from "../types";
import { COUNTRIES, HEIGHT, WIDTH, contains } from "./worldGeometry";

/**
 * Where the folder was photographed, as countries rather than pins.
 *
 * Pins would be the honest reading of the data, but a folder from one trip puts a few
 * hundred of them on one town and says nothing at a glance. Countries answer the
 * question the map is actually asked — where has this camera been — and they survive
 * being drawn at the size of a panel.
 *
 * The boundaries are Natural Earth at 1:110M, which is coarse by design: it keeps the
 * whole world under 110 KB. The cost is at the edges — a photograph taken within a
 * kilometre or so of a border or a coastline can fall on the wrong side, or in the sea.
 */
export function WorldMap({ photos }: { photos: Photo[] }) {
  const { t } = useTranslation();

  const { visited, located } = useMemo(() => {
    /* Tested for a finite number rather than against null: a photo cached by an older
       version of the app carries no position field at all, and `undefined !== null`
       would have let it through to be projected as NaN. */
    const points: [number, number][] = photos
      .filter(
        (p): p is Photo & { lat: number; lon: number } =>
          Number.isFinite(p.lat) && Number.isFinite(p.lon),
      )
      .map((p) => [p.lat, p.lon]);

    const hit = new Set<string>();
    for (const [lat, lon] of points) {
      const country = COUNTRIES.find((c) => contains(c, lon, lat));
      if (country) hit.add(country.id);
    }
    return { visited: hit, located: points.length };
  }, [photos]);

  /* A world map with nothing on it reads as a bug rather than as an answer, so the
     absence is stated instead — and it is the common case: only phones geotag by
     default, and camera raw files never reach this at all. */
  if (located === 0) {
    return <p className="map-empty">{t("stats.mapNone")}</p>;
  }

  /* Each count is its own pluralised key: no language has a plural form that agrees
     with two different numbers at once.

     The two sources are named separately whenever the library contributes, because the
     Overview's own "photos analysed" counts the folder alone — a single total here
     would silently disagree with the figure directly above it, and the folder's photos
     are the only ones this app has actually analysed. */
  const caption = `${t("stats.mapCountries", { count: visited.size })} · ${t(
    "stats.mapPhotos",
    { count: located },
  )}`;

  return (
    <div className="worldmap">
      <svg viewBox={`0 0 ${WIDTH} ${HEIGHT.toFixed(1)}`} role="img" aria-label={caption}>
        {COUNTRIES.map((country) => {
          const on = visited.has(country.id);
          return (
            <path key={country.id} d={country.d} className={on ? "country on" : "country"}>
              {/* Only the highlighted countries get a name: 177 title elements would
                  be dead weight, and the grey ones are not the subject. The names ship
                  in English only — the boundary set has no localised names. */}
              {on && <title>{country.name}</title>}
            </path>
          );
        })}
      </svg>
      <p className="map-caption">{caption}</p>
    </div>
  );
}
