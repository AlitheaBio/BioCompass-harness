import DOMPurify from "dompurify";

/**
 * Strict DOMPurify profile for SVG diagrams (`SEC-21` / `R10`).
 * Forbids `<script>`, `<foreignObject>`, SMIL (`<animate>`, `<set>`), `<style>`, `<desc>`,
 * `<title>`, `xlink:href`, event handlers (`on*`), and non-http(s) URL schemes.
 */
export const SVG_DOMPURIFY_CONFIG = {
  USE_PROFILES: { svg: true, svgFilters: true },
  FORBID_TAGS: [
    "script",
    "foreignobject",
    "iframe",
    "object",
    "embed",
    "applet",
    "animate",
    "animatemotion",
    "animatetransform",
    "set",
    "handler",
    "style",
    "form",
    "input",
    "desc",
    "title",
    "meta",
    "link",
    "base",
  ],
  FORBID_ATTR: [
    "xlink:href",
    "href",
    "src",
    "formaction",
    "action",
    "to",
    "from",
    "values",
    "onload",
    "onerror",
    "onclick",
    "onbegin",
    "onend",
    "onrepeat",
    "onmouseover",
    "onfocus",
  ],
  ALLOW_DATA_ATTR: false,
  ALLOW_UNKNOWN_PROTOCOLS: false,
  ALLOWED_URI_REGEXP: /^https?:\/\//i,
  KEEP_CONTENT: false,
};

const FORBIDDEN_TAG_RE =
  /<\s*\/?\s*(?:script|foreignobject|iframe|object|embed|applet|animate|animatemotion|animatetransform|set|handler|style|form|input|desc|title|meta|link|base)\b/i;
const FORBIDDEN_ATTR_RE =
  /\s(?:on[a-z0-9_-]+|xlink:href|href|formaction|action)\s*=/i;
const FORBIDDEN_SCHEME_RE =
  /(?:javascript|vbscript)\s*:|data\s*:\s*(?:text\/html|image\/svg)/i;

function encodeUtf8Base64(input: string): string {
  const bytes = new TextEncoder().encode(input);
  let binary = "";
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const chunk = bytes.subarray(i, i + chunkSize);
    binary += String.fromCharCode(...chunk);
  }
  if (typeof globalThis.btoa === "function") {
    return globalThis.btoa(binary);
  }
  // Node.js fallback for unit tests
  const bufCtor = (globalThis as { Buffer?: { from: (b: Uint8Array) => { toString: (enc: string) => string } } }).Buffer;
  if (bufCtor) {
    return bufCtor.from(bytes).toString("base64");
  }
  throw new Error("Base64 encoder unavailable.");
}

function stripCommentsAndCdata(markup: string): string {
  return markup
    .replace(/<!\[CDATA\[[\s\S]*?\]\]>/gi, "")
    .replace(/<!--[\s\S]*?-->/g, "")
    .replace(/<!DOCTYPE[^>]*>/gi, "")
    .replace(/<!ENTITY[^>]*>/gi, "");
}

function ensureSvgNamespace(svg: string): string {
  const openTagMatch = svg.match(/^<svg\b([^>]*)>/i);
  if (!openTagMatch) return svg;
  const attrs = openTagMatch[1] ?? "";
  if (/\bxmlns\s*=/i.test(attrs)) {
    return svg;
  }
  return svg.replace(/^<svg\b/i, '<svg xmlns="http://www.w3.org/2000/svg"');
}

function stripForbiddenSvgConstructsFallback(raw: string): string {
  let s = stripCommentsAndCdata(raw.trim());
  const paired = [
    "script",
    "foreignobject",
    "iframe",
    "object",
    "embed",
    "applet",
    "animate",
    "animatemotion",
    "animatetransform",
    "set",
    "handler",
    "style",
    "form",
    "desc",
    "title",
  ];
  for (const tag of paired) {
    const re = new RegExp(`<${tag}\\b[^>]*>[\\s\\S]*?<\\/${tag}\\s*>`, "gi");
    s = s.replace(re, "");
    const selfRe = new RegExp(`<${tag}\\b[^>]*\\/?>`, "gi");
    s = s.replace(selfRe, "");
  }
  s = s.replace(/\s+(?:on[a-z0-9_-]+|xlink:href|href|formaction|action)\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]+)/gi, "");
  return s;
}

/**
 * Sanitize untrusted SVG markup with DOMPurify + structural defense-in-depth and encode
 * as an inert `data:image/svg+xml;base64,...` URI for `<img src={dataUrl} />` (`SEC-21`).
 */
export function sanitizeSvgForImg(rawSvg: string): {
  svg: string | null;
  dataUrl: string | null;
  error: string | null;
} {
  try {
    const trimmed = rawSvg.trim();
    if (!trimmed) {
      return { svg: null, dataUrl: null, error: "SVG markup is empty." };
    }
    const preCleaned = stripCommentsAndCdata(trimmed);
    let cleaned: string;
    if (DOMPurify && DOMPurify.isSupported) {
      cleaned = String(DOMPurify.sanitize(preCleaned, SVG_DOMPURIFY_CONFIG));
    } else {
      cleaned = stripForbiddenSvgConstructsFallback(preCleaned);
    }
    cleaned = stripCommentsAndCdata(cleaned).trim();

    if (!/^<svg\b/i.test(cleaned) || !/<\/svg\s*>$/i.test(cleaned)) {
      return { svg: null, dataUrl: null, error: "Root element must be <svg>." };
    }

    // Defense-in-depth verification after DOMPurify (guards against mXSS, SMIL, event handlers, schemes).
    const compactCheck = cleaned.replace(/[\u0000-\u001f\u007f]/g, "");
    if (
      FORBIDDEN_TAG_RE.test(compactCheck) ||
      FORBIDDEN_ATTR_RE.test(compactCheck) ||
      FORBIDDEN_SCHEME_RE.test(compactCheck.replace(/\s+/g, ""))
    ) {
      return {
        svg: null,
        dataUrl: null,
        error: "SVG diagram contained forbidden active markup and was blocked.",
      };
    }

    const withNs = ensureSvgNamespace(cleaned);
    const dataUrl = `data:image/svg+xml;base64,${encodeUtf8Base64(withNs)}`;
    return { svg: withNs, dataUrl, error: null };
  } catch (err: unknown) {
    return {
      svg: null,
      dataUrl: null,
      error: err instanceof Error ? err.message : "Failed to sanitize SVG diagram.",
    };
  }
}
