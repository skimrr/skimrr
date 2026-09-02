/**
 * The map's geometry, kept free of React so it can be exercised on its own: decoding
 * the topology, projecting it, and answering which country a coordinate falls in.
 * Rendering lives in `WorldMap.tsx`.
 */

import world from "../data/countries-110m.json";

type Point = [number, number];

export interface Country {
  id: string;
  name: string;
  /** Every ring of the country — outer boundaries and holes alike — in lon/lat degrees. */
  rings: Point[][];
  /** `[minLon, minLat, maxLon, maxLat]`, so most countries are rejected per point
      without walking a single edge. */
  bbox: [number, number, number, number];
  /** The projected SVG path, built once: the geometry never changes. */
  d: string;
}

/* The window drawn. Antarctica is deliberately outside it: on an equirectangular
   frame it swallows a third of the height, and nobody's photographs come from there. */
const LON_MIN = -180;
const LAT_MAX = 84;
const LAT_MIN = -58;
export const WIDTH = 1000;
/* One scale for both axes. Stretching them independently to fill a chosen box would
   stop being a projection and start being a distortion. */
const K = WIDTH / 360;
export const HEIGHT = (LAT_MAX - LAT_MIN) * K;

const projectX = (lon: number) => (lon - LON_MIN) * K;
const projectY = (lat: number) => (LAT_MAX - lat) * K;

interface RawGeometry {
  type: string;
  id: string;
  properties: { name: string };
  arcs: unknown;
}

/* TopoJSON stores every shared border exactly once, as an "arc", with its points
   quantised to integer deltas. Decoding undoes both: first the delta encoding, then
   the quantisation, using the topology's own scale and translate. Done here at module
   load, so the 595 arcs are walked once for the life of the app and never per render. */
const ARCS: Point[][] = (() => {
  const { scale, translate } = world.transform;
  return world.arcs.map((arc) => {
    let x = 0;
    let y = 0;
    return arc.map(([dx, dy]) => {
      x += dx;
      y += dy;
      return [x * scale[0] + translate[0], y * scale[1] + translate[1]] as Point;
    });
  });
})();

/* Arc indices are joined end to end. A negative index means "that arc, reversed",
   written as the ones' complement so that arc 0 can be negated at all. Consecutive
   arcs repeat the point they share, which is what `slice(1)` drops. */
function ringFrom(indices: number[]): Point[] {
  const points: Point[] = [];
  for (const index of indices) {
    const arc = index < 0 ? [...ARCS[~index]].reverse() : ARCS[index];
    points.push(...(points.length > 0 ? arc.slice(1) : arc));
  }
  return points;
}

/**
 * Splits a ring wherever it crosses the antimeridian, where longitude steps straight
 * from +180 to -180 in a single edge.
 *
 * Taken literally that edge is a line across the entire width of the map, which is how
 * Russia and Fiji otherwise paint a band over everything between them. It corrupts the
 * containment test for the same reason — those spurious edges are counted by the ray
 * cast — so the fix belongs to the geometry, not to the path string.
 *
 * Each piece closes along the antimeridian itself, since that is where its two ends
 * already sit. The scan is cyclic: a ring's last point joins its first, so the run
 * straddling index 0 is one piece and not two, which is what the rotation arranges.
 */
function splitAtAntimeridian(ring: Point[]): Point[][] {
  const wraps = (a: Point, b: Point) => Math.abs(a[0] - b[0]) > 180;
  const closed =
    ring.length > 2 &&
    ring[0][0] === ring[ring.length - 1][0] &&
    ring[0][1] === ring[ring.length - 1][1];
  const points = closed ? ring.slice(0, -1) : ring;

  let first = -1;
  for (let i = 0; i < points.length; i++) {
    if (wraps(points[(i + points.length - 1) % points.length], points[i])) {
      first = i;
      break;
    }
  }
  if (first === -1) return [ring];

  const rotated = [...points.slice(first), ...points.slice(0, first)];
  const parts: Point[][] = [];
  let current: Point[] = [rotated[0]];
  for (let i = 1; i < rotated.length; i++) {
    if (wraps(rotated[i - 1], rotated[i])) {
      parts.push(current);
      current = [];
    }
    current.push(rotated[i]);
  }
  parts.push(current);
  // Two points cannot enclose an area; a sliver like that is a cut artefact, not land.
  return parts.filter((part) => part.length >= 3);
}

export const COUNTRIES: Country[] = (
  world.objects.countries.geometries as unknown as RawGeometry[]
).map((geometry) => {
  // A Polygon's arcs are a list of rings; a MultiPolygon's are a list of those.
  // Flattening is safe here because holes are handled by the even-odd rule below,
  // which does not care which outer ring a hole belongs to.
  const polygons = (
    geometry.type === "MultiPolygon" ? geometry.arcs : [geometry.arcs]
  ) as number[][][];
  const rings = polygons.flatMap((polygon) =>
    polygon.flatMap((indices) => splitAtAntimeridian(ringFrom(indices))),
  );

  let minLon = 180;
  let minLat = 90;
  let maxLon = -180;
  let maxLat = -90;
  let d = "";
  for (const ring of rings) {
    for (const [lon, lat] of ring) {
      if (lon < minLon) minLon = lon;
      if (lon > maxLon) maxLon = lon;
      if (lat < minLat) minLat = lat;
      if (lat > maxLat) maxLat = lat;
    }
    // One decimal is a tenth of a viewBox unit — far below a rendered pixel, and it
    // keeps the generated path strings to a third of their full-precision size.
    d += ring
      .map(
        ([lon, lat], i) =>
          `${i === 0 ? "M" : "L"}${projectX(lon).toFixed(1)},${projectY(lat).toFixed(1)}`,
      )
      .join("");
    d += "Z";
  }

  return {
    id: geometry.id,
    name: geometry.properties.name,
    rings,
    bbox: [minLon, minLat, maxLon, maxLat],
    d,
  };
});

/**
 * Whether a coordinate falls inside a country, by ray casting: count the edges a ray
 * cast east from the point crosses, and an odd count means inside.
 *
 * Every ring is tested in one pass, holes included — a hole is just another ring, and
 * an even-odd count is precisely what makes a point inside one read as outside the
 * country. The bounding box in front rejects nearly every country in a few comparisons.
 */
export function contains(country: Country, lon: number, lat: number): boolean {
  const [minLon, minLat, maxLon, maxLat] = country.bbox;
  if (lon < minLon || lon > maxLon || lat < minLat || lat > maxLat) return false;

  let inside = false;
  for (const ring of country.rings) {
    for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
      const [xi, yi] = ring[i];
      const [xj, yj] = ring[j];
      if (yi > lat !== yj > lat && lon < ((xj - xi) * (lat - yi)) / (yj - yi) + xi) {
        inside = !inside;
      }
    }
  }
  return inside;
}

