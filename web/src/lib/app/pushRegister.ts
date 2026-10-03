// FCM push-token registration for the Tauri app build.
//
// APP BUILD ONLY — like `deepLinkAuth` and `login`, the push plugin's JS API
// (`@choochmeque/tauri-plugin-notifications-api`) is DYNAMICALLY imported and
// guarded by `IS_APP` so the website build / prerender never touches a Tauri
// API (there is no Tauri runtime there). The plugin's commands only exist
// inside the Android APK; in a plain browser the import is simply skipped.
//
// Flow: request notification permission if needed, ask the plugin for this
// device's FCM token, then upsert it to the backend (`POST /api/push-tokens`)
// so the server can deliver push. Fully guarded — it logs and never throws, so
// a failure here can never break app init / rendering.
import { IS_APP } from '$lib/api/deviceToken';
import { upsertPushToken } from '$lib/api/client';
import { notifications } from '$lib/notificationsState.svelte';

/**
 * Register this device for push and upsert its FCM token to the backend.
 * Returns immediately (a no-op) outside the Tauri app runtime. Call once after
 * the user is authenticated. Never throws — all failures are caught and logged.
 */
export async function registerPushIfTauri(): Promise<void> {
	if (!IS_APP) return;
	try {
		const {
			isPermissionGranted,
			requestPermission,
			registerForPushNotifications,
			onNotificationReceived
		} = await import('@choochmeque/tauri-plugin-notifications-api');

		// Ask for permission only when it isn't already granted.
		let granted = await isPermissionGranted();
		if (!granted) {
			granted = (await requestPermission()) === 'granted';
		}
		if (!granted) return;

		// Android: resolves to the FCM token for this device.
		const token = await registerForPushNotifications();
		if (token) {
			await upsertPushToken('android', token);
		}

		// Refresh the in-app notifications feed when a push arrives while the app
		// is in the foreground, so the bell updates without a manual reload.
		await onNotificationReceived(() => {
			void notifications.refresh();
		});
	} catch (err) {
		console.error('Push registration failed', err);
	}
}
