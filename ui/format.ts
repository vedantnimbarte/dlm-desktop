/** Formatting helpers shared across panels. */

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

/** Binary byte sizes, matching how dlm reports memory. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  // Bytes and KiB never need a decimal; larger units read better with one.
  const digits = unit <= 1 ? 0 : value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

/** Parameter counts: 0.6 B, 7 B, 236 B. */
export function formatParams(count: number): string {
  if (!Number.isFinite(count) || count <= 0) return "—";
  if (count >= 1e9) return `${(count / 1e9).toFixed(count >= 1e10 ? 0 : 1)} B`;
  if (count >= 1e6) return `${(count / 1e6).toFixed(0)} M`;
  return `${count}`;
}

export function formatMs(ms: number | null | undefined): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

/** Clock time for log rows. */
export function formatTime(epochMs: number): string {
  const d = new Date(epochMs);
  return d.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}
