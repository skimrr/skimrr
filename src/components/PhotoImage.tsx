import { useEffect, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { Photo } from "../types";

/**
 * A photo in a grid cell, shown at the best quality actually available for it.
 *
 * For anything walked from disk that is simply its own rendition. For a photo read out
 * of the Photos library it is not: with iCloud storage optimised, the only local copy
 * is a derivative — measured at 480×360 for a frame the library itself reports as
 * 4624×3468. Placed beside a full-size file in a duplicate group, that stand-in makes
 * the better copy look like the worse one, which is the exact opposite of what the
 * comparison is for.
 *
 * So the original is asked of Photos, and a grid-sized rendition built from it. Two
 * things keep that honest:
 *
 * - **Only once the cell is on screen.** Asking Photos for an original downloads it
 *   from iCloud. A library merged into a large scan must never become hundreds of
 *   downloads nobody requested, so nothing is fetched for a group scrolled past.
 * - **Only once.** Both the export and the rendition are cached on disk, so scrolling
 *   back, reopening the tab or rescanning costs nothing.
 *
 * A failure is deliberately silent: the derivative stays on screen. Being unable to
 * reach iCloud is a reason to show a smaller picture, not to show a broken one.
 */
export function PhotoImage({ photo }: { photo: Photo }) {
  const fallback = convertFileSrc(photo.thumb ?? photo.preview);
  const [src, setSrc] = useState(fallback);
  const ref = useRef<HTMLImageElement>(null);

  useEffect(() => {
    setSrc(fallback);
    if (!photo.library) return;
    const element = ref.current;
    if (!element) return;

    let cancelled = false;
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((entry) => entry.isIntersecting)) return;
      // One fetch per photo: disconnect before asking, not after it answers.
      observer.disconnect();
      invoke<string>("library_original", { path: photo.path, thumb: true })
        .then((better) => {
          if (!cancelled && better) setSrc(convertFileSrc(better));
        })
        .catch(() => undefined);
    });
    observer.observe(element);

    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, [photo.path, photo.library, fallback]);

  return (
    <img ref={ref} src={src} alt={photo.name} loading="lazy" decoding="async" />
  );
}
