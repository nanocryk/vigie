import { createContext, memo, type ReactNode, useContext, useMemo } from "react";
import ReactMarkdown, { type Components, type ExtraProps, type Options } from "react-markdown";
import remarkGfm from "remark-gfm";

import { parseFileRef, resolveRefPath, usePathKind } from "@/fileRefs";
import type { PathKind } from "@/files";
import { highlight, languageFromTag } from "@/highlight";

export type PluggableList = NonNullable<Options["remarkPlugins"]>;
export type HastElement = NonNullable<ExtraProps["node"]>;
type HastChild = HastElement["children"][number];

export function textOf(node: HastChild): string {
  if (node.type === "text") return node.value;
  if (node.type === "element") return node.children.map(textOf).join("");
  return "";
}

export function fenceLanguage(code: HastElement): string | null {
  const classes = code.properties["className"];
  const list = Array.isArray(classes) ? classes : [classes];
  for (const name of list) {
    if (typeof name === "string" && name.startsWith("language-")) {
      return languageFromTag(name.slice("language-".length));
    }
  }
  return null;
}

export function CodeBlock({ node }: ExtraProps) {
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

/** Where file paths in inline code resolve and open; null leaves them plain. */
export interface FileLinks {
  cwd: string | null;
  open: (target: PathKind, line: number | null) => void;
}

export const FileLinksContext = createContext<FileLinks | null>(null);

/** Inline code; one naming an existing file or folder opens it in the explorer. */
function InlineCode({ node, children }: { children?: ReactNode } & ExtraProps) {
  const links = useContext(FileLinksContext);
  const text = node === undefined ? "" : textOf(node);
  const ref = useMemo(() => (links === null ? null : parseFileRef(text)), [links, text]);
  const kind = usePathKind(ref === null || links === null ? null : resolveRefPath(ref.path, links.cwd));
  if (links === null || ref === null || kind === null) return <code>{children}</code>;
  const line = kind.kind === "file" ? ref.line : null;
  // An anchor rather than a button, so a long path wraps like the text around it.
  return (
    <a
      href="#"
      className="file-link"
      title={line === null ? kind.path : `${kind.path}:${line}`}
      onClick={(event) => {
        event.preventDefault();
        links.open(kind, line);
      }}
    >
      <code>{children}</code>
    </a>
  );
}

const COMPONENTS: Components = {
  pre: CodeBlock,
  code: InlineCode,
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

const PLUGINS: PluggableList = [remarkGfm];

/**
 * GFM markdown, raw HTML not rendered. `streaming` ends it with a blinking cursor.
 * `plugins` run after GFM; `components` override the defaults. Keep both stable.
 */
export const Markdown = memo(function Markdown({
  text,
  streaming = false,
  plugins,
  components,
}: {
  text: string;
  streaming?: boolean;
  plugins?: PluggableList;
  components?: Components;
}) {
  const allPlugins = useMemo(
    () => (plugins === undefined ? PLUGINS : [...PLUGINS, ...plugins]),
    [plugins],
  );
  const allComponents = useMemo(
    () => (components === undefined ? COMPONENTS : { ...COMPONENTS, ...components }),
    [components],
  );
  return (
    <div className="markdown" data-streaming={streaming ? "" : undefined}>
      <ReactMarkdown remarkPlugins={allPlugins} components={allComponents}>
        {text}
      </ReactMarkdown>
    </div>
  );
});
