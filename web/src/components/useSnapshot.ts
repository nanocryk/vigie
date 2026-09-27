import { useSyncExternalStore } from "react";

import type { Client, Snapshot } from "@/client";

export function useSnapshot(client: Client): Snapshot {
  return useSyncExternalStore(client.subscribe, client.getSnapshot);
}
