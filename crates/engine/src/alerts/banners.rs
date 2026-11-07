//! The engine's own banners, posted while no viewer presents the alerts (the app is closed, or
//! has not said it presents them yet). Plain: monocode's look (the app's name, the chat's title,
//! the words), no actions; a click opens the chat in the app, whose notification delegate reads
//! the alert's identifier.
use onicode_protocol::alerts::Alert;

/// Where the engine posts its own banners. A seam, so no test ever posts one.
pub trait Banners: Send + Sync + 'static {
    /// Shows `alert`, replacing what shows under its id.
    fn post(&self, alert: &Alert, sound: bool);
    /// Takes back what shows under `id`.
    fn withdraw(&self, id: &str);
}

/// Posts nothing: an engine whose viewers present everything, and tests.
pub struct NoBanners;

impl Banners for NoBanners {
    fn post(&self, _alert: &Alert, _sound: bool) {}

    fn withdraw(&self, _id: &str) {}
}

/// The machine's notification center. The engine, which ships inside the app bundle, posts under
/// the app's identity and permission; outside a bundle (a test, `cargo run`) it posts nothing.
pub struct SystemBanners;

impl Banners for SystemBanners {
    fn post(&self, alert: &Alert, sound: bool) {
        #[cfg(target_os = "macos")]
        macos::post(&alert.id, &alert.title, &alert.body, sound);
    }

    fn withdraw(&self, id: &str) {
        #[cfg(target_os = "macos")]
        macos::withdraw(id);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::rc::Retained;
    use objc2_foundation::{NSArray, NSBundle, NSString};
    use objc2_user_notifications::{
        UNMutableNotificationContent, UNNotificationRequest, UNNotificationSound,
        UNUserNotificationCenter,
    };

    /// The category monocode's own banners carry, which the app registers with its "Show"
    /// action.
    const CATEGORY: &str = "onicode.session";

    /// The center, when this process runs inside an app bundle; the center raises outside one.
    fn center() -> Option<Retained<UNUserNotificationCenter>> {
        let bundle = NSBundle::mainBundle();
        bundle.bundleIdentifier()?;
        bundle
            .bundlePath()
            .to_string()
            .ends_with(".app")
            .then(UNUserNotificationCenter::currentNotificationCenter)
    }

    pub(super) fn post(id: &str, title: &str, body: &str, sound: bool) {
        let Some(center) = center() else {
            return;
        };
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str("Onicode"));
        content.setSubtitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        content.setCategoryIdentifier(&NSString::from_str(CATEGORY));
        if sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(id),
            &content,
            None,
        );
        // Without permission the center drops it; the app asks for permission, not the engine.
        center.addNotificationRequest_withCompletionHandler(&request, None);
    }

    pub(super) fn withdraw(id: &str) {
        let Some(center) = center() else {
            return;
        };
        let ids = NSArray::from_retained_slice(&[NSString::from_str(id)]);
        center.removePendingNotificationRequestsWithIdentifiers(&ids);
        center.removeDeliveredNotificationsWithIdentifiers(&ids);
    }
}
