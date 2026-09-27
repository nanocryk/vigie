import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import css from "highlight.js/lib/languages/css";
import ini from "highlight.js/lib/languages/ini";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import markdown from "highlight.js/lib/languages/markdown";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";

hljs.registerLanguage("bash", bash);
hljs.registerLanguage("css", css);
hljs.registerLanguage("ini", ini);
hljs.registerLanguage("javascript", javascript);
hljs.registerLanguage("json", json);
hljs.registerLanguage("markdown", markdown);
hljs.registerLanguage("python", python);
hljs.registerLanguage("rust", rust);
hljs.registerLanguage("typescript", typescript);
hljs.registerLanguage("xml", xml);

/** Fence tags and file extensions, lowercase, to the registered language highlighting them. */
const LANGUAGES: Record<string, string> = {
  rust: "rust",
  rs: "rust",
  typescript: "typescript",
  ts: "typescript",
  tsx: "typescript",
  mts: "typescript",
  cts: "typescript",
  javascript: "javascript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  json: "json",
  jsonc: "json",
  css: "css",
  xml: "xml",
  html: "xml",
  htm: "xml",
  xhtml: "xml",
  svg: "xml",
  python: "python",
  py: "python",
  bash: "bash",
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  toml: "ini",
  ini: "ini",
  markdown: "markdown",
  md: "markdown",
};

/** The language for a code fence tag (`ts`, `rust`, ...), null when not highlighted. */
export function languageFromTag(tag: string): string | null {
  return LANGUAGES[tag.trim().toLowerCase()] ?? null;
}

/** The language for a file path, from its extension; null when not highlighted. */
export function languageFromPath(path: string): string | null {
  const name = path.split(/[\\/]/).pop() ?? "";
  const dot = name.lastIndexOf(".");
  return dot <= 0 ? null : languageFromTag(name.slice(dot + 1));
}

/** Highlighted HTML (text escaped, tokens in `hljs-*` spans), null for an unknown language. */
export function highlight(code: string, language: string | null): string | null {
  if (language === null || hljs.getLanguage(language) === undefined) return null;
  return hljs.highlight(code, { language, ignoreIllegals: true }).value;
}

/**
 * Splits highlighted HTML at its newlines into one self-contained fragment per
 * line: spans open across a newline are closed at the line's end and reopened
 * on the next one.
 */
export function splitHighlighted(html: string): string[] {
  const lines: string[] = [];
  const open: string[] = [];
  let line = "";
  for (const [token] of html.matchAll(/<span[^>]*>|<\/span>|[^<]+|</g)) {
    if (token.startsWith("<span")) {
      open.push(token);
      line += token;
    } else if (token === "</span>") {
      open.pop();
      line += token;
    } else {
      const parts = token.split("\n");
      line += parts[0] ?? "";
      for (const part of parts.slice(1)) {
        lines.push(line + "</span>".repeat(open.length));
        line = open.join("") + part;
      }
    }
  }
  lines.push(line);
  return lines;
}

/** `highlight` split per line, null for an unknown language. */
export function highlightLines(code: string, language: string | null): string[] | null {
  const html = highlight(code, language);
  return html === null ? null : splitHighlighted(html);
}
