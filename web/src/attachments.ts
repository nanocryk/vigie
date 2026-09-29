import { isRecord } from "@/wire";

/**
 * The text Claude gets for `text` with `attachments`, as the server writes it
 * (`session::attachments::compose`): the text, a blank line, then one `- path`
 * line per file under `Attachments:`; the block alone when the text is blank.
 */
export function composeText(text: string, attachments: readonly string[]): string {
  if (attachments.length === 0) return text;
  const list = attachments.map((path) => `\n- ${path}`).join("");
  return text.trim() === "" ? `Attachments:${list}` : `${text}\n\nAttachments:${list}`;
}

/** A draft's text to send, or null when it holds neither text nor files. */
export function sendableText(text: string, attachments: readonly string[]): string | null {
  return text.trim() === "" && attachments.length === 0 ? null : text;
}

/** Spread into a command or an action: `attachments` when there are some. */
export function attachmentsField(attachments: readonly string[]): { attachments?: string[] } {
  return attachments.length === 0 ? {} : { attachments: [...attachments] };
}

const IMAGE = /\.(png|jpe?g|gif|webp|bmp|avif|ico)$/i;

export function isImagePath(path: string): boolean {
  return IMAGE.test(path);
}

/** The file name, without the random prefix the server gives uploads. */
export function attachmentName(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? path;
  return name.replace(/^[0-9a-f]{12}-/, "");
}

/** Stores `file` in the session's uploads folder and returns its path on the host. */
export async function uploadFile(session: string, file: File): Promise<string> {
  const name = file.name === "" ? "pasted" : file.name;
  const response = await fetch(
    `api/sessions/${encodeURIComponent(session)}/upload?name=${encodeURIComponent(name)}`,
    {
      method: "POST",
      headers: { "Content-Type": file.type === "" ? "application/octet-stream" : file.type },
      body: file,
    },
  );
  const body: unknown = await response.json().catch(() => null);
  if (!response.ok) {
    const code = isRecord(body) && typeof body["error"] === "string" ? body["error"] : null;
    throw new Error(code ?? `HTTP ${response.status}`);
  }
  if (!isRecord(body) || typeof body["path"] !== "string") {
    throw new Error(`upload: no path in ${JSON.stringify(body)}`);
  }
  return body["path"];
}
