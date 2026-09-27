import { reload, useNewVersion } from "@/version";

/** Pull-to-refresh does not exist in the installed app, so reloading is a button. */
export function ReloadButton() {
  return (
    <button
      type="button"
      onClick={reload}
      aria-label="Reload"
      title="Reload"
      className="text-muted-foreground hover:bg-muted voice-ui shrink-0 rounded-[5px] px-2 text-sm leading-6"
    >
      &#x21bb;
    </button>
  );
}

export function NewVersionBanner() {
  const stale = useNewVersion();
  if (!stale) return null;
  return (
    <button
      type="button"
      onClick={reload}
      className="bg-primary text-primary-foreground voice-ui w-full shrink-0 px-3 py-1.5 text-sm"
    >
      New version available: tap to reload
    </button>
  );
}
