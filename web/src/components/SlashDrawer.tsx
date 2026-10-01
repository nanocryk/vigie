import type { CommandCard } from "@/commands";
import type { Drawer, Suggestion } from "@/completion";

function MatchedName({ suggestion }: { suggestion: Suggestion }) {
  const matched = new Set(suggestion.indexes);
  return (
    <span className="voice-code shrink-0 text-sm">
      /
      {[...suggestion.card.name].map((char, index) =>
        matched.has(index) ? (
          <span key={index} className="text-primary font-semibold">
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
    <div className="bg-background absolute inset-x-0 bottom-full z-20 mb-1 overflow-hidden rounded-[5px] border shadow-lg">
      {drawer.kind === "suggest" ? (
        <ul role="listbox">
          {drawer.suggestions.map((suggestion, index) => (
            <li key={suggestion.card.name} role="option" aria-selected={index === selected}>
              <button
                type="button"
                onMouseDown={(event) => event.preventDefault()}
                onClick={() => onPick(suggestion.card)}
                className={`hover:bg-muted flex w-full cursor-pointer items-baseline gap-2 px-3 py-1.5 text-left ${index === selected ? "bg-muted" : ""}`}
              >
                <MatchedName suggestion={suggestion} />
                <span className="text-muted-foreground voice-ui min-w-0 truncate text-xs">
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
          <span className="voice-code shrink-0 text-sm">/{drawer.card.name}</span>
          <span className="text-muted-foreground voice-code min-w-0 truncate text-xs">
            {drawer.card.argumentHint}
          </span>
        </button>
      )}
    </div>
  );
}
