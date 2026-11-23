import { useEffect, useMemo, useState } from "react";
import { IconCheck, IconCopy, IconDownload } from "./Icons";
import { sanitizeSvgForImg } from "../svgSanitize";

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

export function sanitizeSvgMarkup(rawSvg: string): {
  svg: string | null;
  dataUrl: string | null;
  error: string | null;
} {
  return sanitizeSvgForImg(rawSvg);
}

function downloadSvg(svgMarkup: string, filenameBase: string) {
  const blob = new Blob([svgMarkup], { type: "application/octet-stream" });
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
  const [renderedDataUrl, setRenderedDataUrl] = useState<string | null>(null);
  const [renderError, setRenderError] = useState<string | null>(null);

  const sanitizedDirectSvg = useMemo(() => {
    if (kind !== "svg" || !closed) return null;
    return sanitizeSvgForImg(source);
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
        const cleaned = sanitizeSvgForImg(svg);
        if (!cancelled) {
          if (cleaned.svg && cleaned.dataUrl) {
            setRenderedSvg(cleaned.svg);
            setRenderedDataUrl(cleaned.dataUrl);
            setRenderError(null);
          } else {
            setRenderedSvg(null);
            setRenderedDataUrl(null);
            setRenderError(cleaned.error ?? "Unable to sanitize diagram output.");
          }
        }
      } catch (err: unknown) {
        if (!cancelled) {
          setRenderedSvg(null);
          setRenderedDataUrl(null);
          setRenderError(
            err instanceof Error ? err.message : "Mermaid diagram syntax error.",
          );
        }
      } finally {
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
  const dataUrl =
    kind === "svg" ? (sanitizedDirectSvg?.dataUrl ?? null) : renderedDataUrl;
  const effectiveError =
    kind === "svg" ? (sanitizedDirectSvg?.error ?? null) : renderError;

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
      ) : dataUrl ? (
        <div
          className="bc-md-diagram-canvas"
          onClick={() => {
            if (dataUrl && onOpenImage) {
              onOpenImage(dataUrl, `${kind.toUpperCase()} Diagram`);
            }
          }}
          title={onOpenImage ? "Click to zoom diagram" : undefined}
        >
          <img
            src={dataUrl}
            alt={`${kind.toUpperCase()} Diagram`}
            style={{ maxWidth: "100%", height: "auto", display: "block" }}
          />
        </div>
      ) : (
        <div className="bc-md-diagram-status">
          <span className="bc-caption">Preparing diagram...</span>
        </div>
      )}
    </div>
  );
}
