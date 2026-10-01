# vigie

- A running server keeps its copy of the binary until restarted. After a server change, run `canif auto server/build` and see it pass before asking for a restart or calling `restart_server`. A restart before the build reloads the old binary.
- `web/dist` is the live app. Rebuild it with `canif auto web/build` once a feature under `web/` is finished and `web/check` passes, without being asked. Never build mid-feature: a half-done change would break the live app.
