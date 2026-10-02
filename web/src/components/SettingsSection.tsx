import { type ReactNode, useId } from "react";

import { ChevronIcon } from "@/components/Explorer";
import { toggleSection, useOpenSections } from "@/sectionsOpen";

/**
 * A Device settings card, closed by default to a header of title and summary.
 * `action` stays in the header, usable while closed. `forceOpen` holds it open
 * while something needs a look. Without a body, the header shows alone.
 */
export function SettingsSection({
  id,
  title,
  summary,
  action,
  forceOpen = false,
  children,
}: {
  id: string;
  title: string;
  summary: ReactNode;
  action?: ReactNode;
  forceOpen?: boolean;
  children?: ReactNode;
}) {
  const stored = useOpenSections().has(id);
  const bodyId = useId();
  const hasBody = children !== undefined && children !== null && children !== false;
  const open = hasBody && (forceOpen || stored);
  const heading = (
    <span className="min-w-0">
      <span className="block text-sm voice-heading">{title}</span>
      <span className="block text-xs voice-ui text-muted-foreground">{summary}</span>
    </span>
  );
  return (
    <section className="rounded-[5px] border bg-card">
      <div className="flex items-center justify-between gap-3 px-4 py-2">
        {hasBody ? (
          <h2 className="min-w-0 flex-1">
            <button
              type="button"
              aria-expanded={open}
              aria-controls={bodyId}
              disabled={forceOpen}
              onClick={() => toggleSection(id)}
              className="flex w-full items-start gap-2 text-left"
            >
              <ChevronIcon
                className={`mt-px size-4 shrink-0 text-muted-foreground transition-transform ${open ? "" : "-rotate-90"}`}
              />
              {heading}
            </button>
          </h2>
        ) : (
          <h2 className="flex min-w-0 flex-1 gap-2">
            <span className="size-4 shrink-0" />
            {heading}
          </h2>
        )}
        {action}
      </div>
      {open && (
        <div id={bodyId} className="space-y-3 px-4 pb-4">
          {children}
        </div>
      )}
    </section>
  );
}
