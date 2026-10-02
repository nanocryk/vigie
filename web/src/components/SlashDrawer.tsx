import type { CommandCard } from "@/commands";
import type { Drawer, Suggestion } from "@/completion";

function MatchedName({ suggestion }: { suggestion: Suggestion }) {
  const matched = new Set(suggestion.indexes);
  return (
    <span className="shrink-0 text-sm voice-code">
      /
      {[...suggestion.card.name].map((char, index) =>
        matched.has(index) ? (
          <span key={index} className="font-semibold text-primary">
            {char}
          </span>
        ) : (
          char
        ),
      )}
    </span>
  );
}

/** Above the composer input: the commands a `/word` may complete to, or a command's argument help. */
export function SlashDrawer({
  drawer,
  selected,
  onPick,
  onDismiss,
}: {
  drawer: Exclude<Drawer, { kind: "none" }>;
  selected: number;
  onPick: (card: CommandCard) => void;
  onDismiss: () => void;
}) {
  return (
    <div className="absolute inset-x-0 bottom-full z-20 mb-1 overflow-hidden rounded-[5px] border bg-background shadow-lg">
      {drawer.kind === "suggest" ? (
        <ul role="listbox">
          {drawer.suggestions.map((suggestion, index) => (
            <li key={suggestion.card.name} role="option" aria-selected={index === selected}>
              <button
                type="button"
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => onPick(suggestion.card)}
                className={`flex w-full cursor-pointer items-baseline gap-2 px-3 py-1.5 text-left hover:bg-muted ${index === selected ? "bg-muted" : ""}`}
              >
                <MatchedName suggestion={suggestion} />
                <span className="min-w-0 truncate text-xs voice-ui text-muted-foreground">
                  {suggestion.card.description}
                </span>
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <button
          type="button"
          onMouseDown={(event) => event.preventDefault()}
          onClick={onDismiss}
          className="flex w-full cursor-pointer items-baseline gap-2 px-3 py-1.5 text-left"
        >
          <span className="shrink-0 text-sm voice-code">/{drawer.card.name}</span>
          <span className="min-w-0 truncate text-xs voice-code text-muted-foreground">
            {drawer.card.argumentHint}
          </span>
        </button>
      )}
    </div>
  );
}
