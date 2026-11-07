use std::cell::RefCell;
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, MainThreadMarker, define_class};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSArray, NSBundle, NSError, NSSet, NSString};
use objc2_user_notifications::*;
use onicode_protocol::alerts::{self, Alert, AlertAction};

use super::{AlertResponse, Notification, Permission, Target, resolved};

const CATEGORY: &str = "onicode.session";
const SHOW: &str = "onicode.session.show";
static CLICKS: Mutex<VecDeque<Target>> = Mutex::new(VecDeque::new());
/// Agent alert buttons pressed, until the shell's poll takes them.
static ALERT_RESPONSES: Mutex<VecDeque<AlertResponse>> = Mutex::new(VecDeque::new());

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "OnicodeNotificationDelegate"]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            completion: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            completion.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn clicked(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion: &block2::DynBlock<dyn Fn()>,
        ) {
            let action = response.actionIdentifier();
            if let Some(button) = AlertAction::from_id(&action.to_string()) {
                // An agent alert's button: the shell makes its call; the app stays behind.
                let text = response
                    .downcast_ref::<UNTextInputNotificationResponse>()
                    .map(|reply| reply.userText().to_string());
                if let Ok(mut queue) = ALERT_RESPONSES.lock() {
                    queue.push_back(AlertResponse {
                        id: response.notification().request().identifier().to_string(),
                        action: button,
                        text,
                    });
                }
            } else if (action.to_string() == SHOW
                || action.isEqualToString(unsafe { UNNotificationDefaultActionIdentifier }))
                && let Some(target) =
                    Target::from_tag(&response.notification().request().identifier().to_string())
                && let Ok(mut queue) = CLICKS.lock()
            {
                queue.push_back(target);
            }
            completion.call(());
        }
    }
);

thread_local! {
    // The center retains its delegate weakly. Keep ours alive for the process.
    static DELEGATE: RefCell<Option<Retained<Delegate>>> = const { RefCell::new(None) };
}

fn center() -> Option<Retained<UNUserNotificationCenter>> {
    // Unbundled cargo-run executables make this API raise an ObjC exception.
    NSBundle::mainBundle().bundleIdentifier()?;
    Some(UNUserNotificationCenter::currentNotificationCenter())
}

pub fn initialize() -> bool {
    if MainThreadMarker::new().is_none() {
        return false;
    }
    let Some(center) = center() else {
        return false;
    };
    DELEGATE.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        let delegate: Retained<Delegate> = unsafe { objc2::msg_send![Delegate::alloc(), init] };
        center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        *slot.borrow_mut() = Some(delegate);
        let show = UNNotificationAction::actionWithIdentifier_title_options(
            &NSString::from_str(SHOW),
            &NSString::from_str("Show"),
            UNNotificationActionOptions::Foreground,
        );
        let category =
            UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
                &NSString::from_str(CATEGORY),
                &NSArray::from_retained_slice(&[show]),
                &NSArray::new(),
                UNNotificationCategoryOptions::empty(),
            );
        let mut categories = vec![category];
        categories.extend(
            alerts::ACTION_SETS
                .iter()
                .map(|actions| alert_category(actions)),
        );
        center.setNotificationCategories(&NSSet::from_retained_slice(&categories));
    });
    true
}

/// The category of one set of agent alert buttons. None brings the app forward; Reply opens a
/// text field, and Stop shows as destructive.
fn alert_category(actions: &[AlertAction]) -> Retained<UNNotificationCategory> {
    let buttons: Vec<Retained<UNNotificationAction>> = actions
        .iter()
        .map(|action| {
            let id = NSString::from_str(action.id());
            let title = NSString::from_str(action.label());
            match action {
                AlertAction::Reply => Retained::into_super(
                    UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
                        &id,
                        &title,
                        UNNotificationActionOptions::empty(),
                        &NSString::from_str("Send"),
                        &NSString::from_str("Reply"),
                    ),
                ),
                AlertAction::Stop => UNNotificationAction::actionWithIdentifier_title_options(
                    &id,
                    &title,
                    UNNotificationActionOptions::Destructive,
                ),
                AlertAction::Approve | AlertAction::Deny => {
                    UNNotificationAction::actionWithIdentifier_title_options(
                        &id,
                        &title,
                        UNNotificationActionOptions::empty(),
                    )
                }
            }
        })
        .collect();
    UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
        &NSString::from_str(&alerts::category(actions)),
        &NSArray::from_retained_slice(&buttons),
        &NSArray::new(),
        UNNotificationCategoryOptions::empty(),
    )
}

fn map_settings(settings: &UNNotificationSettings) -> Permission {
    match settings.authorizationStatus() {
        UNAuthorizationStatus::NotDetermined => Permission::Prompt,
        UNAuthorizationStatus::Denied => Permission::Denied,
        _ if settings.alertSetting() == UNNotificationSetting::Disabled
            || settings.alertStyle() == UNAlertStyle::None =>
        {
            Permission::Denied
        }
        _ => Permission::Granted,
    }
}

pub fn permission() -> Receiver<Permission> {
    let Some(center) = center() else {
        return resolved(Permission::Unsupported);
    };
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        let _ = sender.send(map_settings(unsafe { settings.as_ref() }));
    });
    center.getNotificationSettingsWithCompletionHandler(&block);
    receiver
}

/// Only call from an explicit opt-in in Settings. Sending never requests permission.
pub fn request_permission() -> Receiver<Permission> {
    let Some(center) = center() else {
        return resolved(Permission::Unsupported);
    };
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
        let Some(center) = self::center() else {
            let _ = sender.send(Permission::Unsupported);
            return;
        };
        let sender = sender.clone();
        let query = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            let _ = sender.send(map_settings(unsafe { settings.as_ref() }));
        });
        center.getNotificationSettingsWithCompletionHandler(&query);
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge,
        &block,
    );
    receiver
}

/// Result arrives only when the OS accepts/rejects dispatch. The host should
/// wait at most five seconds in a background task and then play its fallback.
pub fn show(notification: Notification) -> Receiver<Result<(), String>> {
    let Some(center) = center() else {
        return resolved(Err("Notifications require an app bundle".into()));
    };
    let (sender, receiver) = mpsc::channel();
    let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        if map_settings(unsafe { settings.as_ref() }) != Permission::Granted {
            let _ = sender.send(Err("Notifications are not authorized".into()));
            return;
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str("Onicode"));
        content.setSubtitle(&NSString::from_str(&notification.subtitle));
        content.setBody(&NSString::from_str(&notification.body));
        content.setCategoryIdentifier(&NSString::from_str(CATEGORY));
        if notification.sound {
            content.setSound(Some(&UNNotificationSound::defaultSound()));
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&notification.tag),
            &content,
            None,
        );
        let sender = sender.clone();
        let completion = RcBlock::new(move |error: *mut NSError| {
            let result = unsafe { error.as_ref() }
                .map(|error| Err(error.localizedDescription().to_string()))
                .unwrap_or(Ok(()));
            let _ = sender.send(result);
        });
        if let Some(center) = self::center() {
            center.addNotificationRequest_withCompletionHandler(&request, Some(&completion));
        }
    });
    center.getNotificationSettingsWithCompletionHandler(&block);
    receiver
}

/// Posts an agent alert in monocode's look (the app's name, the chat's title, the words) with
/// its buttons, replacing what shows under its identifier. The center drops it without
/// permission; the shell asked at opt-in.
pub fn show_alert(alert: &Alert, sound: bool) {
    let Some(center) = center() else {
        return;
    };
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("Onicode"));
    content.setSubtitle(&NSString::from_str(&alert.title));
    content.setBody(&NSString::from_str(
        &crate::notifications::delivery::clip_body(&alert.body),
    ));
    content.setCategoryIdentifier(&NSString::from_str(&alert.category()));
    if sound {
        content.setSound(Some(&UNNotificationSound::defaultSound()));
    }
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str(&alert.id),
        &content,
        None,
    );
    center.addNotificationRequest_withCompletionHandler(&request, None);
}

/// The agent alert buttons pressed since the last call.
pub fn drain_alert_responses() -> Vec<AlertResponse> {
    ALERT_RESPONSES
        .lock()
        .map(|mut queue| queue.drain(..).collect())
        .unwrap_or_default()
}

pub fn drain_clicks() -> Vec<Target> {
    CLICKS
        .lock()
        .map(|mut queue| queue.drain(..).collect())
        .unwrap_or_default()
}

pub fn set_dock_badge(count: usize) {
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let label = (count > 0).then(|| NSString::from_str(&count.to_string()));
    NSApplication::sharedApplication(main)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

pub fn dismiss(tag: &str) {
    if let Some(center) = center() {
        let identifiers = NSArray::from_retained_slice(&[NSString::from_str(tag)]);
        center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
        center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "opens the viewer's own System Settings"
)]
pub fn open_settings() -> Result<(), String> {
    let identifier = NSBundle::mainBundle()
        .bundleIdentifier()
        .ok_or("Notifications require an app bundle")?;
    std::process::Command::new("open")
        .arg(format!(
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={identifier}"
        ))
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}
