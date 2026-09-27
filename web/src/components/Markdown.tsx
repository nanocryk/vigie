import { memo } from "react";
import ReactMarkdown, { type Components, type ExtraProps } from "react-markdown";
import remarkGfm from "remark-gfm";

import { highlight, languageFromTag } from "@/highlight";

type HastElement = NonNullable<ExtraProps["node"]>;
type HastChild = HastElement["children"][number];

function textOf(node: HastChild): string {
  if (node.type === "text") return node.value;
  if (node.type === "element") return node.children.map(textOf).join("");
  return "";
}

function fenceLanguage(code: HastElement): string | null {
  const classes = code.properties["className"];
  const list = Array.isArray(classes) ? classes : [classes];
  for (const name of list) {
    if (typeof name === "string" && name.startsWith("language-")) {
      return languageFromTag(name.slice("language-".length));
    }
  }
  return null;
}

function CodeBlock({ node }: ExtraProps) {
  if (node === undefined) return null;
  const code = node.children.find(
    (child): child is HastElement => child.type === "element" && child.tagName === "code",
  );
  const text = (code === undefined ? textOf(node) : textOf(code)).replace(/\n$/, "");
  const html = highlight(text, code === undefined ? null : fenceLanguage(code));
  return (
    <pre>
      {html === null ? <code>{text}</code> : <code dangerouslySetInnerHTML={{ __html: html }} />}
    </pre>
  );
}

const COMPONENTS: Components = {
  pre: CodeBlock,
  a: ({ href, title, children }) => (
    <a href={href} title={title} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  ),
  table: ({ children }) => (
    <div className="md-table">
      <table>{children}</table>
    </div>
  ),
};

const PLUGINS = [remarkGfm];

/** GFM markdown, raw HTML not rendered. `streaming` ends it with a blinking cursor. */
export const Markdown = memo(function Markdown({
  text,
  streaming = false,
}: {
  text: string;
  streaming?: boolean;
}) {
  return (
    <div className="markdown" data-streaming={streaming ? "" : undefined}>
      <ReactMarkdown remarkPlugins={PLUGINS} components={COMPONENTS}>
        {text}
      </ReactMarkdown>
    </div>
  );
});
