/** This browser can receive Web Push. */
export function pushSupported(): boolean {
  return "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
}

export interface PushConfig {
  vapidPublicKey: string;
  /** The server holds a subscription for this device. */
  subscribed: boolean;
}

export function parsePushConfig(raw: unknown, label: string): PushConfig {
  if (typeof raw !== "object" || raw === null) {
    throw new Error(`${label}: expected an object: ${JSON.stringify(raw)}`);
  }
  const key: unknown = Reflect.get(raw, "vapid_public_key");
  const subscribed: unknown = Reflect.get(raw, "subscribed");
  if (typeof key !== "string" || key === "") {
    throw new Error(`${label}.vapid_public_key: expected a string: ${JSON.stringify(key)}`);
  }
  if (typeof subscribed !== "boolean") {
    throw new Error(`${label}.subscribed: expected a boolean: ${JSON.stringify(subscribed)}`);
  }
  return { vapidPublicKey: key, subscribed };
}

/** The raw bytes of a base64url string, as `applicationServerKey` takes them. */
export function base64UrlBytes(text: string): Uint8Array<ArrayBuffer> {
  const base64 = text.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4));
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

async function check(response: Response): Promise<void> {
  if (response.ok) return;
  const text = await response.text().catch(() => "");
  throw new Error(`HTTP ${response.status}${text === "" ? "" : `: ${text}`}`);
}

export async function fetchPushConfig(): Promise<PushConfig> {
  const response = await fetch("api/push/config");
  await check(response);
  return parsePushConfig(await response.json(), "push config");
}

/** Asks the notification permission, subscribes this browser and registers it. */
export async function enablePush(config: PushConfig): Promise<void> {
  const permission = await Notification.requestPermission();
  if (permission !== "granted") throw new Error(`Notifications are ${permission} in this browser`);
  const registration = await navigator.serviceWorker.ready;
  const subscription = await registration.pushManager.subscribe({
    userVisibleOnly: true,
    applicationServerKey: base64UrlBytes(config.vapidPublicKey),
  });
  await check(
    await fetch("api/push/subscribe", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(subscription.toJSON()),
    }),
  );
}

export async function disablePush(): Promise<void> {
  const registration = await navigator.serviceWorker.ready;
  const subscription = await registration.pushManager.getSubscription();
  await subscription?.unsubscribe();
  await check(await fetch("api/push/unsubscribe", { method: "POST" }));
}

export async function sendTestPush(): Promise<void> {
  await check(await fetch("api/push/test", { method: "POST" }));
}

/** Closes this device's notification of discussion `session`, if one is shown. */
export async function clearNotification(session: string): Promise<void> {
  if (!("serviceWorker" in navigator)) return;
  const registration = await navigator.serviceWorker.getRegistration();
  const shown = (await registration?.getNotifications({ tag: session })) ?? [];
  for (const notification of shown) notification.close();
}
