/**
 * Validate external / federated URLs before rendering in `<a href>` or `window.location.assign`
 * (`SEC-09`, `SEC-32`). Only `https:`, `http:`, and optional single-slash relative `/...` paths
 * are allowed; rejects control characters, `javascript:`, `vbscript:`, `data:`, `//`, `/\`.
 */
export function isSafeHttpUrl(
  raw: string | null | undefined,
  options?: { allowRelative?: boolean },
): string | null {
  if (typeof raw !== "string") {
    return null;
  }
  const trimmed = raw.trim();
  if (!trimmed) {
    return null;
  }
  for (let i = 0; i < trimmed.length; i++) {
    const code = trimmed.charCodeAt(i);
    if (code <= 0x1f || code === 0x7f) {
      return null;
    }
  }
  if (trimmed.startsWith("/")) {
    if (options?.allowRelative === false) {
      return null;
    }
    if (trimmed.startsWith("//") || trimmed.startsWith("/\\")) {
      return null;
    }
    return trimmed;
  }
  try {
    const parsed = new URL(trimmed);
    if (parsed.protocol === "https:" || parsed.protocol === "http:") {
      return parsed.toString();
    }
    return null;
  } catch {
    return null;
  }
}

export function sanitizeFederatedLaunchUrl(raw: string): string | null {
  return isSafeHttpUrl(raw, { allowRelative: true });
}

export const safeLaunchUrl = sanitizeFederatedLaunchUrl;

const SAFE_RASTER_DATA_IMAGE_RE =
  /^data:image\/(?:png|jpeg|jpg|gif|webp);base64,[A-Za-z0-9+/=\s]+$/i;

/**
 * Restrict Markdown `![alt](src)` images to inert raster `data:image/*;base64,` URIs or
 * same-origin `/api/` paths (`SEC-27`). Blocks ALL external `http(s)://` URLs and SVG data URIs
 * to prevent zero-click data exfiltration and script execution.
 */
export function isSafeMarkdownImageUrl(raw: string | null | undefined): boolean {
  if (typeof raw !== "string") return false;
  const trimmed = raw.trim();
  if (!trimmed) return false;
  for (let i = 0; i < trimmed.length; i++) {
    const code = trimmed.charCodeAt(i);
    if (code <= 0x1f || code === 0x7f) return false;
  }
  if (SAFE_RASTER_DATA_IMAGE_RE.test(trimmed)) {
    return true;
  }
  if (
    trimmed.startsWith("/api/") &&
    !trimmed.startsWith("//") &&
    !trimmed.startsWith("/\\") &&
    !/\s/.test(trimmed)
  ) {
    return true;
  }
  return false;
}

/**
 * Validate Markdown `[label](href)` links (`https:`, `http:`, or same-origin `/api/`).
 */
export function isSafeMarkdownLinkUrl(raw: string | null | undefined): boolean {
  const safe = isSafeHttpUrl(raw, { allowRelative: true });
  if (!safe) return false;
  if (safe.startsWith("/")) {
    return safe.startsWith("/api/");
  }
  return true;
}
