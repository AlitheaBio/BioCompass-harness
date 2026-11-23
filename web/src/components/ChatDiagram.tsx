import { useEffect, useMemo, useState } from "react";
import { IconCheck, IconCopy, IconDownload } from "./Icons";

let mermaidQueue: Promise<unknown> = Promise.resolve();
let diagramRenderSeq = 0;

function useIsDarkTheme(): boolean {
  const [dark, setDark] = useState<boolean>(() =>
    typeof document !== "undefined"
      ? document.body?.getAttribute("data-theme") === "dark" ||
        document.documentElement.getAttribute("data-theme") === "dark"
      : false,
  );

  useEffect(() => {
    if (typeof document === "undefined") return;
    const sync = () => {
      setDark(
        document.body?.getAttribute("data-theme") === "dark" ||
          document.documentElement.getAttribute("data-theme") === "dark",
      );
    };
    sync();
    const obs = new MutationObserver(sync);
    if (document.body) {
      obs.observe(document.body, {
        attributes: true,
        attributeFilter: ["data-theme"],
      });
    }
    obs.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => obs.disconnect();
  }, []);

  return dark;
}

const DISALLOWED_SVG_TAGS = new Set([
  "script",
  "foreignobject",
  "iframe",
  "object",
  "embed",
  "applet",
]);

export function sanitizeSvgMarkup(
  rawSvg: string,
  options?: { allowForeignObject?: boolean },
): { svg: string | null; error: string | null } {
  try {
    const parser = new DOMParser();
    let doc = parser.parseFromString(rawSvg.trim(), "image/svg+xml");
    let root: Element | null = doc.querySelector("parsererror")
      ? null
      : doc.documentElement;

    // Mermaid can emit HTML fragments (e.g. <br> inside <foreignObject>) that parse cleanly
    // under HTML5 even when strict XML rejects unclosed tags.
    if (!root || root.tagName.toLowerCase() !== "svg") {
      doc = parser.parseFromString(rawSvg.trim(), "text/html");
      root = doc.body.querySelector("svg");
    }

    if (!root || root.tagName.toLowerCase() !== "svg") {
      return { svg: null, error: "Root element must be <svg>." };
    }

    const walk = (node: Element) => {
      for (const child of Array.from(node.children)) {
        const tag = child.tagName.toLowerCase();
        if (
          DISALLOWED_SVG_TAGS.has(tag) &&
          !(options?.allowForeignObject && tag === "foreignobject")
        ) {
          child.remove();
          continue;
        }
        walk(child);
      }
      for (const attr of Array.from(node.attributes)) {
        const name = attr.name.toLowerCase();
        const val = attr.value.trim();
        if (name.startsWith("on")) {
          node.removeAttribute(attr.name);
        } else if (
          (name === "href" || name === "xlink:href" || name === "src") &&
          /^(javascript:|vbscript:|data:text\/html)/i.test(val)
        ) {
          node.removeAttribute(attr.name);
        }
      }
    };

    walk(root);
    if (!root.getAttribute("xmlns")) {
      root.setAttribute("xmlns", "http://www.w3.org/2000/svg");
    }
    return { svg: new XMLSerializer().serializeToString(root), error: null };
  } catch (err: unknown) {
    return {
      svg: null,
      error: err instanceof Error ? err.message : "Failed to parse SVG diagram.",
    };
  }
}

function downloadSvg(svgMarkup: string, filenameBase: string) {
  const blob = new Blob([svgMarkup], { type: "image/svg+xml;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `${filenameBase}.svg`;
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function ChatDiagram({
  kind,
  source,
  closed = true,
  onOpenImage,
}: {
  kind: "mermaid" | "svg";
  source: string;
  closed?: boolean;
  onOpenImage?: (src: string, alt?: string) => void;
}) {
  const isDark = useIsDarkTheme();
  const [viewMode, setViewMode] = useState<"preview" | "source">("preview");
  const [copied, setCopied] = useState(false);
  const [renderedSvg, setRenderedSvg] = useState<string | null>(null);
  const [renderError, setRenderError] = useState<string | null>(null);

  const sanitizedDirectSvg = useMemo(() => {
    if (kind !== "svg" || !closed) return null;
    return sanitizeSvgMarkup(source);
  }, [kind, source, closed]);

  useEffect(() => {
    if (kind !== "mermaid" || !closed || !source.trim()) {
      return;
    }
    let cancelled = false;
    const renderId = `bc-mermaid-${++diagramRenderSeq}`;

    const job = mermaidQueue.then(async () => {
      if (cancelled) return;
      try {
        const mod = await import("mermaid");
        const mermaid = mod.default;
        mermaid.initialize({
          startOnLoad: false,
          securityLevel: "strict",
          htmlLabels: false,
          flowchart: { htmlLabels: false },
          theme: isDark ? "dark" : "default",
          fontFamily: "system-ui, -apple-system, BlinkMacSystemFont, sans-serif",
          themeVariables: isDark
            ? {
                darkMode: true,
                background: "transparent",
                primaryColor: "#2c1f45",
                primaryTextColor: "#f3f0fa",
                primaryBorderColor: "#a855f7",
                lineColor: "#b8a8dc",
                edgeLabelBackground: "#1b1429",
                nodeBorder: "#a855f7",
                clusterBkg: "#1e162e",
                clusterBorder: "#6d48a8",
                secondaryColor: "#1b1429",
                tertiaryColor: "#2c2240",
              }
            : {
                darkMode: false,
                background: "transparent",
                primaryColor: "#fff4ef",
                primaryTextColor: "#1c1917",
                primaryBorderColor: "#e8612d",
                lineColor: "#57534e",
                edgeLabelBackground: "#faf8f5",
                secondaryColor: "#faf8f5",
                tertiaryColor: "#f5f2ed",
              },
        });
        const { svg } = await mermaid.render(renderId, source.trim());
        const cleaned = sanitizeSvgMarkup(svg, { allowForeignObject: true });
        if (!cancelled) {
          if (cleaned.svg) {
            setRenderedSvg(cleaned.svg);
            setRenderError(null);
          } else {
            setRenderedSvg(null);
            setRenderError(cleaned.error ?? "Unable to sanitize diagram output.");
          }
        }
      } catch (err: unknown) {
        if (!cancelled) {
          setRenderedSvg(null);
          setRenderError(
            err instanceof Error ? err.message : "Mermaid diagram syntax error.",
          );
        }
      } finally {
        // Mermaid sometimes leaves temporary scratch containers in document.body on error.
        document.getElementById(`d${renderId}`)?.remove();
        document.getElementById(renderId)?.remove();
      }
    });

    mermaidQueue = job.catch(() => undefined);

    return () => {
      cancelled = true;
    };
  }, [kind, source, closed, isDark]);

  const effectiveSvg =
    kind === "svg" ? (sanitizedDirectSvg?.svg ?? null) : renderedSvg;
  const effectiveError =
    kind === "svg" ? (sanitizedDirectSvg?.error ?? null) : renderError;

  const dataUrl = useMemo(() => {
    if (!effectiveSvg) return null;
    return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(effectiveSvg)}`;
  }, [effectiveSvg]);

  return (
    <div
      className="bc-md-diagram-card"
      data-testid="chat-diagram-card"
      data-diagram-kind={kind}
    >
      <div className="bc-md-code-head">
        <span className="bc-md-code-lang mono">Diagram · {kind}</span>
        <div className="bc-md-code-actions">
          <button
            type="button"
            className="bc-md-code-btn"
            onClick={() =>
              setViewMode((m) => (m === "preview" ? "source" : "preview"))
            }
          >
            <span>{viewMode === "preview" ? "Source" : "Preview"}</span>
          </button>
          {effectiveSvg && (
            <button
              type="button"
              className="bc-md-code-btn"
              onClick={() => downloadSvg(effectiveSvg, `biocompass-${kind}-diagram`)}
              title="Download diagram as SVG"
            >
              <IconDownload size={12} />
              <span>SVG</span>
            </button>
          )}
          <button
            type="button"
            className="bc-md-code-btn"
            onClick={() => {
              void navigator.clipboard?.writeText(source);
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1500);
            }}
            title="Copy diagram source"
          >
            {copied ? <IconCheck size={12} /> : <IconCopy size={12} />}
            <span>{copied ? "Copied" : "Copy"}</span>
          </button>
        </div>
      </div>

      {viewMode === "source" ? (
        <pre className="bc-md-pre mono">
          <code>{source}</code>
        </pre>
      ) : !closed ? (
        <div className="bc-md-diagram-status">
          <span className="bc-caption">Rendering {kind} diagram...</span>
        </div>
      ) : effectiveError ? (
        <div className="bc-md-diagram-error">
          <div className="bc-api-error-note" style={{ marginTop: 0 }}>
            Diagram render failed: {effectiveError}
          </div>
          <pre className="bc-md-pre mono">
            <code>{source}</code>
          </pre>
        </div>
      ) : effectiveSvg ? (
        <div
          className="bc-md-diagram-canvas"
          onClick={() => {
            if (dataUrl && onOpenImage) {
              onOpenImage(dataUrl, `${kind.toUpperCase()} Diagram`);
            }
          }}
          title={onOpenImage ? "Click to zoom diagram" : undefined}
          dangerouslySetInnerHTML={{ __html: effectiveSvg }}
        />
      ) : (
        <div className="bc-md-diagram-status">
          <span className="bc-caption">Preparing diagram...</span>
        </div>
      )}
    </div>
  );
}
