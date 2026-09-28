// Scope is the app's base path. No fetch handling: installability and push.

self.addEventListener("install", () => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

// Payload `{ title, body, tag, url }`; one notification per tag, a new one replacing it.
self.addEventListener("push", (event) => {
  let message = { title: "vigie2", body: "", tag: "vigie2", url: "" };
  try {
    message = { ...message, ...event.data.json() };
  } catch {
    // An unreadable payload still shows, so the push is never silent.
  }
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
