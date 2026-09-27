import { StrictMode, Suspense } from "react";
import { createRoot } from "react-dom/client";

import { App } from "@/App";
import { bootstrap } from "@/pairing";
import "@/index.css";

const rootEl = document.getElementById("root");
if (rootEl === null) throw new Error("missing #root element");

// Started once, outside React: the startup token is single-use, so a repeated
// effect run would spend it and then report it as invalid.
const boot = bootstrap({
  hash: location.hash,
  fetch: (input, init) => fetch(input, init),
  stripHash: () => history.replaceState(null, "", location.pathname + location.search),
});

createRoot(rootEl).render(
  <StrictMode>
    <Suspense fallback={null}>
      <App boot={boot} />
    </Suspense>
  </StrictMode>,
);
