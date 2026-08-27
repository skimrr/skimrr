export interface Photo {
  path: string;
  name: string;
  size: number;
  width: number;
  height: number;
  taken: number;
  blur: number | null;
  /** What the webview can display: the file itself, or a cached rendition. */
  preview: string;
  /** A much smaller rendition for grid cells — use this over `preview` wherever a
      photo is shown small (a cover, a review grid), falling back to `preview` when
      absent (an older cache entry, or a Photos-library photo, whose own cached
      derivative is already thumbnail-sized). Never use it for a full-screen view. */
  thumb: string | null;
  /** Uppercase extension for camera raw files (ARW, CR2…), else null. */
  kind: string | null;
}

export interface Group {
  indices: number[];
  suggested: number;
  kind: "exact" | "similar" | "pair";
  /** Which criterion decided the suggestion, absent when nothing separated them. */
  reason: "raw" | "pixels" | "sharp" | "recent" | null;
  similarity: number;
}

export interface View {
  photos: Photo[];
  groups: Group[];
  reclaimable_bytes: number;
  total_files: number;
}

export interface TrashResult {
  batch_id: string;
  count: number;
}

export interface PhotosComparison {
  already_in_photos: string[];
  missing_from_photos: string[];
}

export interface TrashedPhoto {
  stored_path: string;
  preview: string;
  original: string;
  name: string;
  size: number;
}

export interface TrashBatch {
  batch_id: string;
  when: number;
  photos: TrashedPhoto[];
  bytes: number;
}

export function formatBytes(bytes: number, lang: string): string {
  const units = ["byte", "kilobyte", "megabyte", "gigabyte"] as const;
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return new Intl.NumberFormat(lang, {
    style: "unit",
    unit: units[unit],
    maximumFractionDigits: value >= 100 || unit === 0 ? 0 : 1,
  }).format(value);
}

export function formatDate(unixSeconds: number, lang: string): string {
  if (!unixSeconds) return "";
  return new Intl.DateTimeFormat(lang, { dateStyle: "medium" }).format(
    new Date(unixSeconds * 1000),
  );
}

/** Follow the system, or force one of the two. */
export type Theme = "auto" | "light" | "dark";

export interface Licence {
  activated: boolean;
  status: string;
  activation_usage: number;
  activation_limit: number;
  /** Set when an activation attempt failed, so the screen can explain itself. */
  message: string | null;
  /** Newer version announced by the licence receipt, when there is one. */
  update_available: string | null;
}
