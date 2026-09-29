import { reload, useNewVersion } from "@/version";

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
