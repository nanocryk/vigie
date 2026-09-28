// Scope is the app's base path. No fetch handling: installability and push.

self.addEventListener("install", () => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

// Payload `{ title, body, tag, url }`: one notification per tag, a new one
// replacing it. Payload `{ clear: tag }`: closes that tag and shows nothing,
// once the discussion was seen on some device.
self.addEventListener("push", (event) => {
  let raw = null;
  try {
    raw = event.data.json();
  } catch {
    // An unreadable payload still shows, so the push is never silent.
  }
  if (raw !== null && typeof raw.clear === "string") {
    event.waitUntil(
      self.registration
        .getNotifications({ tag: raw.clear })
        .then((shown) => shown.forEach((notification) => notification.close())),
    );
    return;
  }
  const message = { title: "vigie2", body: "", tag: "vigie2", url: "", ...raw };
  event.waitUntil(
    self.registration.showNotification(message.title, {
      body: message.body,
      tag: message.tag,
      renotify: true,
      data: { url: new URL(message.url, self.registration.scope).href },
    }),
  );
});

// Focuses an open vigie2 window on the discussion, else opens one.
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = event.notification.data?.url ?? self.registration.scope;
  event.waitUntil(
    (async () => {
      const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
      const open = windows.find((client) => client.url.startsWith(self.registration.scope));
      if (open !== undefined) {
        await open.focus();
        await open.navigate(url);
        return;
      }
      await self.clients.openWindow(url);
    })(),
  );
});
