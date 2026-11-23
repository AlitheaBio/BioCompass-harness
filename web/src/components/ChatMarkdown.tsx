import React, { useState } from "react";
import { ChatDiagram } from "./ChatDiagram";
import { IconCheck, IconCopy, IconDownload, IconPlus } from "./Icons";

type TableAlign = "left" | "center" | "right";

type MdBlock =
  | { type: "heading"; level: number; text: string }
  | { type: "code"; lang: string; code: string; closed: boolean }
  | { type: "svg"; code: string; closed: boolean }
  | { type: "math"; latex: string }
  | { type: "table"; headers: string[]; alignments: TableAlign[]; rows: string[][] }
  | { type: "blockquote"; lines: string[] }
  | { type: "list"; ordered: boolean; items: { text: string; checked?: boolean }[] }
  | { type: "hr" }
  | { type: "paragraph"; lines: string[] };

const SAFE_URL_RE = /^(https?:\/\/|data:image\/|\/api\/|\.\/)/i;

function isSafeUrl(url: string): boolean {
  return SAFE_URL_RE.test(url.trim());
}

function downloadImage(src: string, alt?: string) {
  const link = document.createElement("a");
  link.href = src;
  const cleanName = (alt || "biocompass-figure")
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-|-$/g, "");
  const extMatch = src.match(/^data:image\/([a-z0-9+]+);/i);
  const ext = extMatch?.[1] === "jpeg" ? "jpg" : extMatch?.[1]?.split("+")[0] ?? "png";
  link.download = `${cleanName || "biocompass-figure"}.${ext}`;
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
}

function splitTableRow(line: string): string[] {
  let trimmed = line.trim();
  if (trimmed.startsWith("|")) trimmed = trimmed.slice(1);
  if (trimmed.endsWith("|")) trimmed = trimmed.slice(0, -1);
  const cells: string[] = [];
  let cur = "";
  let escaped = false;
  for (let i = 0; i < trimmed.length; i++) {
    const ch = trimmed[i]!;
    if (escaped) {
      cur += ch;
      escaped = false;
      continue;
    }
    if (ch === "\\") {
      escaped = true;
      continue;
    }
    if (ch === "|") {
      cells.push(cur.trim());
      cur = "";
    } else {
      cur += ch;
    }
  }
  cells.push(cur.trim());
  return cells;
}

function parseTableSeparator(line: string): TableAlign[] | null {
  const trimmed = line.trim();
  if (!trimmed.includes("|") && !trimmed.includes("-")) return null;
  const cols = splitTableRow(trimmed);
  if (cols.length < 1) return null;
  const aligns: TableAlign[] = [];
  for (const col of cols) {
    if (!/^:?-{2,}:?$/.test(col)) return null;
    if (col.startsWith(":") && col.endsWith(":")) aligns.push("center");
    else if (col.endsWith(":")) aligns.push("right");
    else aligns.push("left");
  }
  return aligns;
}

function parseMarkdownBlocks(raw: string): MdBlock[] {
  const lines = raw.replace(/\r\n/g, "\n").split("\n");
  const blocks: MdBlock[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i]!;
    const trimmed = line.trim();

    if (!trimmed) {
      i++;
      continue;
    }

    // Fenced code blocks ```lang or ~~~lang
    const fenceMatch = trimmed.match(/^(```+|~~~+)\s*([a-zA-Z0-9_+#.-]*)\s*$/);
    if (fenceMatch) {
      const fence = fenceMatch[1]!;
      const fenceChar = fence[0]!;
      const lang = fenceMatch[2] || "";
      const codeLines: string[] = [];
      let closed = false;
      i++;
      while (i < lines.length) {
        const curTrim = lines[i]!.trim();
        if (curTrim.startsWith(fenceChar.repeat(fence.length)) && curTrim === fenceChar.repeat(curTrim.length)) {
          closed = true;
          i++;
          break;
        }
        codeLines.push(lines[i]!);
        i++;
      }
      blocks.push({ type: "code", lang, code: codeLines.join("\n"), closed });
      continue;
    }

    // Raw inline <svg>...</svg> block (when LLM emits SVG outside code fences)
    if (/^<svg(?:\s|>|$)/i.test(trimmed)) {
      const svgLines: string[] = [line];
      let closed = /<\/svg>/i.test(line);
      i++;
      while (!closed && i < lines.length) {
        svgLines.push(lines[i]!);
        if (/<\/svg>/i.test(lines[i]!)) {
          closed = true;
          i++;
          break;
        }
        i++;
      }
      blocks.push({ type: "svg", code: svgLines.join("\n"), closed });
      continue;
    }

    // Display math $$ ... $$
    if (trimmed.startsWith("$$")) {
      const rest = trimmed.slice(2).trim();
      if (rest.endsWith("$$") && rest.length > 2) {
        blocks.push({ type: "math", latex: rest.slice(0, -2).trim() });
        i++;
        continue;
      }
      const mathLines: string[] = rest ? [rest] : [];
      i++;
      while (i < lines.length) {
        const curTrim = lines[i]!.trim();
        if (curTrim.endsWith("$$")) {
          const prefix = curTrim.slice(0, -2).trim();
          if (prefix) mathLines.push(prefix);
          i++;
          break;
        }
        mathLines.push(lines[i]!);
        i++;
      }
      blocks.push({ type: "math", latex: mathLines.join("\n") });
      continue;
    }

    // Horizontal rule
    if (/^(-{3,}|\*{3,}|_{3,})$/.test(trimmed)) {
      blocks.push({ type: "hr" });
      i++;
      continue;
    }

    // Heading
    const headingMatch = trimmed.match(/^(#{1,6})\s+(.*)$/);
    if (headingMatch) {
      blocks.push({
        type: "heading",
        level: headingMatch[1]!.length,
        text: headingMatch[2]!.trim(),
      });
      i++;
      continue;
    }

    // GFM Table (header row followed by |---|---| separator row)
    if (line.includes("|") && i + 1 < lines.length) {
      const aligns = parseTableSeparator(lines[i + 1]!);
      if (aligns && aligns.length >= 1) {
        const headers = splitTableRow(line);
        i += 2;
        const rows: string[][] = [];
        while (i < lines.length) {
          const rowLine = lines[i]!;
          if (!rowLine.trim() || !rowLine.includes("|")) break;
          rows.push(splitTableRow(rowLine));
          i++;
        }
        blocks.push({ type: "table", headers, alignments: aligns, rows });
        continue;
      }
    }

    // Blockquote
    if (/^\s*>\s?/.test(line)) {
      const quoteLines: string[] = [];
      while (i < lines.length && /^\s*>\s?/.test(lines[i]!)) {
        quoteLines.push(lines[i]!.replace(/^\s*>\s?/, ""));
        i++;
      }
      blocks.push({ type: "blockquote", lines: quoteLines });
      continue;
    }

    // Unordered / Task / Ordered list
    const ulStart = line.match(/^\s*[-*+]\s+(.*)$/);
    const olStart = line.match(/^\s*\d+[.)]\s+(.*)$/);
    if (ulStart || olStart) {
      const ordered = Boolean(olStart);
      const items: { text: string; checked?: boolean }[] = [];
      while (i < lines.length) {
        const cur = lines[i]!;
        const match = ordered
          ? cur.match(/^\s*\d+[.)]\s+(.*)$/)
          : cur.match(/^\s*[-*+]\s+(.*)$/);
        if (!match) break;
        let text = match[1]!;
        let checked: boolean | undefined;
        const taskMatch = !ordered ? text.match(/^\[( |x|X)\]\s+(.*)$/) : null;
        if (taskMatch) {
          checked = taskMatch[1]!.toLowerCase() === "x";
          text = taskMatch[2]!;
        }
        i++;
        while (
          i < lines.length &&
          lines[i]!.trim() !== "" &&
          !/^\s*([-*+]|\d+[.)]|>|#{1,6}\s|```|~~~|\$\$)/.test(lines[i]!) &&
          /^\s{2,}/.test(lines[i]!)
        ) {
          text += " " + lines[i]!.trim();
          i++;
        }
        items.push({ text, checked });
      }
      blocks.push({ type: "list", ordered, items });
      continue;
    }

    // Paragraph until blank line or next block opener
    const paraLines: string[] = [];
    while (i < lines.length) {
      const cur = lines[i]!;
      const curTrim = cur.trim();
      if (!curTrim) break;
      if (
        paraLines.length > 0 &&
        (/^(```+|~~~+|#{1,6}\s|\s*>\s?|\$\$|(-{3,}|\*{3,}|_{3,})$)/.test(curTrim) ||
          /^\s*[-*+]\s+/.test(cur) ||
          /^\s*\d+[.)]\s+/.test(cur) ||
          (cur.includes("|") && i + 1 < lines.length && parseTableSeparator(lines[i + 1]!) !== null))
      ) {
        break;
      }
      paraLines.push(cur);
      i++;
    }
    blocks.push({ type: "paragraph", lines: paraLines });
  }

  return blocks;
}

export function ChatMarkdown({
  content,
  canInsertNotebook,
  onInsertCode,
  onOpenImage,
}: {
  content: string;
  canInsertNotebook?: boolean;
  onInsertCode?: (code: string, lang: string) => void;
  onOpenImage?: (src: string, alt?: string) => void;
}) {
  const [copiedKey, setCopiedKey] = useState<string | null>(null);

  const renderInline = (text: string, keyPrefix: string): React.ReactNode[] => {
    const out: React.ReactNode[] = [];
    const tokenRe =
      /(`[^`\n]+`|!\[[^\]]*\]\([^)]+\)|\[[^\]]+\]\([^)]+\)|\*\*\*[^*\n]+\*\*\*|\*\*[^*\n]+\*\*|__[^_\n]+__|~~[^~\n]+~~|\*[^*\n]+\*)/g;
    let lastIndex = 0;
    let match: RegExpExecArray | null;
    let idx = 0;

    while ((match = tokenRe.exec(text)) !== null) {
      if (match.index > lastIndex) {
        out.push(text.slice(lastIndex, match.index));
      }
      const tok = match[0]!;
      const key = `${keyPrefix}-${idx++}`;

      if (tok.startsWith("`") && tok.endsWith("`")) {
        out.push(
          <code key={key} className="bc-md-inline-code">
            {tok.slice(1, -1)}
          </code>,
        );
      } else if (tok.startsWith("![")) {
        const imgMatch = tok.match(/^!\[([^\]]*)\]\(([^)]+)\)$/);
        if (imgMatch && isSafeUrl(imgMatch[2]!)) {
          const alt = imgMatch[1] || "Figure";
          const src = imgMatch[2]!.trim();
          out.push(
            <figure key={key} className="bc-md-image-figure">
              <img
                src={src}
                alt={alt}
                className="bc-md-image"
                onClick={() => onOpenImage?.(src, alt)}
              />
              <figcaption className="bc-md-image-cap">
                <span>{alt}</span>
                <button
                  type="button"
                  className="bc-btn bc-btn-ghost bc-btn-sm"
                  onClick={() => downloadImage(src, alt)}
                  title="Download image locally"
                >
                  <IconDownload size={12} />
                  <span>Save</span>
                </button>
              </figcaption>
            </figure>,
          );
        } else {
          out.push(tok);
        }
      } else if (tok.startsWith("[")) {
        const linkMatch = tok.match(/^\[([^\]]+)\]\(([^)]+)\)$/);
        if (linkMatch && isSafeUrl(linkMatch[2]!)) {
          const label = linkMatch[1]!;
          const href = linkMatch[2]!.trim();
          out.push(
            <a
              key={key}
              href={href}
              target={href.startsWith("http") ? "_blank" : undefined}
              rel={href.startsWith("http") ? "noopener noreferrer" : undefined}
              className="bc-md-link"
            >
              {label}
            </a>,
          );
        } else {
          out.push(tok);
        }
      } else if (tok.startsWith("***") && tok.endsWith("***")) {
        out.push(
          <strong key={key}>
            <em>{renderInline(tok.slice(3, -3), key)}</em>
          </strong>,
        );
      } else if (
        (tok.startsWith("**") && tok.endsWith("**")) ||
        (tok.startsWith("__") && tok.endsWith("__"))
      ) {
        out.push(<strong key={key}>{renderInline(tok.slice(2, -2), key)}</strong>);
      } else if (tok.startsWith("~~") && tok.endsWith("~~")) {
        out.push(<del key={key}>{renderInline(tok.slice(2, -2), key)}</del>);
      } else if (tok.startsWith("*") && tok.endsWith("*")) {
        out.push(<em key={key}>{renderInline(tok.slice(1, -1), key)}</em>);
      } else {
        out.push(tok);
      }
      lastIndex = tokenRe.lastIndex;
    }

    if (lastIndex < text.length) {
      out.push(text.slice(lastIndex));
    }
    return out;
  };

  const blocks = parseMarkdownBlocks(content);

  return (
    <div className="bc-md">
      {blocks.map((block, bi) => {
        if (block.type === "heading") {
          const inner = renderInline(block.text, `h-${bi}`);
          if (block.level === 1) return <h1 key={bi} className="bc-md-h1">{inner}</h1>;
          if (block.level === 2) return <h2 key={bi} className="bc-md-h2">{inner}</h2>;
          if (block.level === 3) return <h3 key={bi} className="bc-md-h3">{inner}</h3>;
          return <h4 key={bi} className="bc-md-h4">{inner}</h4>;
        }

        if (block.type === "svg") {
          return (
            <ChatDiagram
              key={bi}
              kind="svg"
              source={block.code}
              closed={block.closed}
              onOpenImage={onOpenImage}
            />
          );
        }

        if (block.type === "code") {
          const normLang = block.lang.trim().toLowerCase();
          if (normLang === "mermaid" || normLang === "mermaidjs") {
            return (
              <ChatDiagram
                key={bi}
                kind="mermaid"
                source={block.code}
                closed={block.closed}
                onOpenImage={onOpenImage}
              />
            );
          }
          if (normLang === "svg" && /<svg[\s>]/i.test(block.code)) {
            return (
              <ChatDiagram
                key={bi}
                kind="svg"
                source={block.code}
                closed={block.closed}
                onOpenImage={onOpenImage}
              />
            );
          }
          const copyId = `code-${bi}`;
          const isCopied = copiedKey === copyId;
          const langLabel = block.lang || "code";
          const showNotebookBtn =
            Boolean(canInsertNotebook && onInsertCode) &&
            (block.lang === "" ||
              /^(python|py|jupyter|ipython|sh|bash|r)$/i.test(block.lang));
          return (
            <div key={bi} className="bc-md-code-wrap">
              <div className="bc-md-code-head">
                <span className="bc-md-code-lang mono">{langLabel}</span>
                <div className="bc-md-code-actions">
                  {showNotebookBtn && (
                    <button
                      type="button"
                      className="bc-md-code-btn"
                      onClick={() => onInsertCode?.(block.code, langLabel)}
                      title="Insert as a new cell in Jupyter Notebook and open split view"
                    >
                      <IconPlus size={12} />
                      <span>Add to Notebook</span>
                    </button>
                  )}
                  <button
                    type="button"
                    className="bc-md-code-btn"
                    onClick={() => {
                      void navigator.clipboard?.writeText(block.code);
                      setCopiedKey(copyId);
                      window.setTimeout(() => {
                        setCopiedKey((cur) => (cur === copyId ? null : cur));
                      }, 1500);
                    }}
                    title="Copy code"
                  >
                    {isCopied ? <IconCheck size={12} /> : <IconCopy size={12} />}
                    <span>{isCopied ? "Copied" : "Copy"}</span>
                  </button>
                </div>
              </div>
              <pre className="bc-md-pre mono">
                <code>{block.code}</code>
              </pre>
            </div>
          );
        }

        if (block.type === "math") {
          return (
            <pre key={bi} className="bc-md-math mono">
              {block.latex}
            </pre>
          );
        }

        if (block.type === "table") {
          return (
            <div key={bi} className="bc-md-table-wrap">
              <table className="bc-md-table">
                <thead>
                  <tr>
                    {block.headers.map((h, ci) => (
                      <th
                        key={ci}
                        style={{ textAlign: block.alignments[ci] ?? "left" }}
                      >
                        {renderInline(h, `th-${bi}-${ci}`)}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {block.rows.map((row, ri) => (
                    <tr key={ri}>
                      {block.headers.map((_, ci) => (
                        <td
                          key={ci}
                          style={{ textAlign: block.alignments[ci] ?? "left" }}
                        >
                          {renderInline(row[ci] ?? "", `td-${bi}-${ri}-${ci}`)}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          );
        }

        if (block.type === "blockquote") {
          return (
            <blockquote key={bi} className="bc-md-quote">
              {block.lines.map((line, li) => (
                <div key={li}>{renderInline(line, `q-${bi}-${li}`)}</div>
              ))}
            </blockquote>
          );
        }

        if (block.type === "list") {
          const Tag = block.ordered ? "ol" : "ul";
          return (
            <Tag key={bi} className="bc-md-list">
              {block.items.map((item, ii) => (
                <li key={ii}>
                  {item.checked !== undefined && (
                    <input
                      type="checkbox"
                      checked={item.checked}
                      readOnly
                      className="bc-md-checkbox"
                    />
                  )}
                  <span>{renderInline(item.text, `li-${bi}-${ii}`)}</span>
                </li>
              ))}
            </Tag>
          );
        }

        if (block.type === "hr") {
          return <hr key={bi} className="bc-md-hr" />;
        }

        return (
          <p key={bi} className="bc-md-p">
            {block.lines.map((line, li) => (
              <React.Fragment key={li}>
                {li > 0 && <br />}
                {renderInline(line, `p-${bi}-${li}`)}
              </React.Fragment>
            ))}
          </p>
        );
      })}
    </div>
  );
}
