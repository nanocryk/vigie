import { StrictMode, Suspense } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/App";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import { startDisplay } from "@/display";
import { bootstrap } from "@/pairing";
import "@/index.css";

startDisplay();

const rootEl = document.getElementById("root");
if (rootEl === null) throw new Error("missing #root element");

// Started once, outside React: the startup token is single-use, so a repeated
// effect run would spend it and then report it as invalid.
const boot = bootstrap({
  hash: location.hash,
  fetch: (input, init) => fetch(input, init),
  stripHash: () => history.replaceState(null, "", location.pathname + location.search),
});

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("./sw.js").catch((err: unknown) => {
    console.error("service worker registration failed", err);
  });
}

createRoot(rootEl).render(
  <StrictMode>
    <ErrorBoundary>
      <Suspense fallback={null}>
        <App boot={boot} />
      </Suspense>
    </ErrorBoundary>
  </StrictMode>,
);
