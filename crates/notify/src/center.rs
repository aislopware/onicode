//! Agent alerts through `UNUserNotificationCenter`: banners with buttons and a reply field,
//! replaced and withdrawn by id (`docs/onicode/decisions/alerts.md`).
//!
//! Only the app uses this; the background engine keeps the plain banners in the crate root. The
//! center throws for a process outside an app bundle (`cargo run`, the engine binary), so every
//! call is a no-op there. Authorization was asked at onboarding; without it the center drops the
//! posts and says so in the log.

/// A button on an alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub style: Style,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Style {
    Plain,
    /// Shown in red: it ends something.
    Destructive,
    /// Opens a text field; the typed words come back in [`Response::text`].
    Reply {
        button: String,
        placeholder: String,
    },
}

/// A set of buttons, registered up front and named by each alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    pub actions: Vec<Action>,
}

/// One alert. Posting another with the same `id` replaces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    /// Alerts of one thread group together.
    pub thread: String,
    pub title: String,
    pub body: String,
    pub category: String,
    /// Opaque to this crate; handed back in [`Response::info`].
    pub info: String,
    /// Breaks through a Focus.
    pub time_sensitive: bool,
}

/// What the owner did with an alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub id: String,
    /// The button's id, or `None` for a click on the alert itself.
    pub action: Option<String>,
    /// What was typed into a [`Style::Reply`] field.
    pub text: Option<String>,
    pub info: Option<String>,
}

type Handler = Box<dyn Fn(Response) + Send>;

static HANDLER: std::sync::Mutex<Option<Handler>> = std::sync::Mutex::new(None);

/// Become the center's delegate and register `categories`. Call on the main thread while the app
/// finishes launching, so a click that launched the app is not missed. `handler` runs on
/// whichever thread the center answers on. False outside an app bundle.
pub fn install(categories: &[Category], handler: impl Fn(Response) + Send + 'static) -> bool {
    *HANDLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(handler));
    imp::install(categories)
}

/// Show `note`, replacing one with the same id.
pub fn post(note: &Note) {
    if std::env::var_os(crate::DISABLE_ENV).is_some() {
        return;
    }
    imp::post(note);
}

/// Take these alerts down, shown or not yet shown.
pub fn withdraw(ids: &[String]) {
    if !ids.is_empty() {
        imp::withdraw(ids);
    }
}

fn respond(response: Response) {
    if let Some(handler) = HANDLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        handler(response);
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::{AllocAnyThread, define_class, msg_send};
    use objc2_foundation::{
        NSArray, NSBundle, NSDictionary, NSError, NSObject, NSObjectProtocol, NSSet, NSString,
    };
    use objc2_user_notifications::{
        UNMutableNotificationContent, UNNotification, UNNotificationAction,
        UNNotificationActionOptions, UNNotificationCategory, UNNotificationCategoryOptions,
        UNNotificationInterruptionLevel, UNNotificationPresentationOptions, UNNotificationRequest,
        UNNotificationResponse, UNTextInputNotificationAction, UNTextInputNotificationResponse,
        UNUserNotificationCenter, UNUserNotificationCenterDelegate,
    };

    use super::{Category, Note, Response, Style};

    /// The `userInfo` key that carries [`Note::info`].
    const INFO: &str = "info";

    /// The center, when this process is an app bundle with an id.
    fn center() -> Option<Retained<UNUserNotificationCenter>> {
        let bundle = NSBundle::mainBundle();
        let in_app = bundle.bundleIdentifier().is_some()
            && bundle.bundlePath().to_string().ends_with(".app");
        in_app.then(UNUserNotificationCenter::currentNotificationCenter)
    }

    define_class!(
        // SAFETY:
        // - `NSObject` may be subclassed.
        // - `Delegate` has no ivars and does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "OnicodeAlertCenterDelegate"]
        struct Delegate;

        unsafe impl NSObjectProtocol for Delegate {}

        unsafe impl UNUserNotificationCenterDelegate for Delegate {
            // Shown while the app is in front too: the engine already left out the chat on
            // screen, and the app chimes on its own.
            #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
            fn will_present(
                &self,
                _center: &UNUserNotificationCenter,
                _notification: &UNNotification,
                handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
            ) {
                handler.call((UNNotificationPresentationOptions::Banner
                    | UNNotificationPresentationOptions::List,));
            }

            #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
            fn did_receive(
                &self,
                _center: &UNUserNotificationCenter,
                response: &UNNotificationResponse,
                handler: &block2::DynBlock<dyn Fn()>,
            ) {
                super::respond(read(response));
                handler.call(());
            }
        }
    );

    impl Delegate {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(());
            // SAFETY: `NSObject`'s `init` on the allocation `set_ivars` just filled.
            unsafe { msg_send![super(this), init] }
        }
    }

    fn read(response: &UNNotificationResponse) -> Response {
        let request = response.notification().request();
        let action = response.actionIdentifier().to_string();
        // Apple's own ids: the alert itself was clicked (or dismissed).
        let action = (!action.starts_with("com.apple.")).then_some(action);
        let text = response
            .downcast_ref::<UNTextInputNotificationResponse>()
            .map(|reply| reply.userText().to_string());
        let info = request.content().userInfo();
        // SAFETY: only reads a value by a string key, and checks its class before use.
        let info: &NSDictionary<NSString, AnyObject> = unsafe { info.cast_unchecked() };
        let info = info
            .objectForKey(&NSString::from_str(INFO))
            .and_then(|value| value.downcast::<NSString>().ok())
            .map(|value| value.to_string());
        Response {
            id: request.identifier().to_string(),
            action,
            text,
            info,
        }
    }

    pub(super) fn install(categories: &[Category]) -> bool {
        static INSTALLED: AtomicBool = AtomicBool::new(false);
        let Some(center) = center() else {
            return false;
        };
        if !INSTALLED.swap(true, Ordering::SeqCst) {
            let delegate = Delegate::new();
            center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            // The center holds its delegate weakly; this one lives as long as the app.
            std::mem::forget(delegate);
        }
        let categories: Vec<Retained<UNNotificationCategory>> =
            categories.iter().map(category).collect();
        center.setNotificationCategories(&NSSet::from_retained_slice(&categories));
        true
    }

    fn category(category: &Category) -> Retained<UNNotificationCategory> {
        let actions: Vec<Retained<UNNotificationAction>> = category
            .actions
            .iter()
            .map(|action| {
                let id = NSString::from_str(&action.id);
                let title = NSString::from_str(&action.title);
                match &action.style {
                    Style::Plain => UNNotificationAction::actionWithIdentifier_title_options(
                        &id,
                        &title,
                        UNNotificationActionOptions::empty(),
                    ),
                    Style::Destructive => UNNotificationAction::actionWithIdentifier_title_options(
                        &id,
                        &title,
                        UNNotificationActionOptions::Destructive,
                    ),
                    Style::Reply {
                        button,
                        placeholder,
                    } => Retained::into_super(
                        UNTextInputNotificationAction::actionWithIdentifier_title_options_textInputButtonTitle_textInputPlaceholder(
                            &id,
                            &title,
                            UNNotificationActionOptions::empty(),
                            &NSString::from_str(button),
                            &NSString::from_str(placeholder),
                        ),
                    ),
                }
            })
            .collect();
        UNNotificationCategory::categoryWithIdentifier_actions_intentIdentifiers_options(
            &NSString::from_str(&category.id),
            &NSArray::from_retained_slice(&actions),
            &NSArray::<NSString>::from_slice(&[]),
            UNNotificationCategoryOptions::empty(),
        )
    }

    pub(super) fn post(note: &Note) {
        let Some(center) = center() else {
            return;
        };
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(&note.title));
        content.setBody(&NSString::from_str(&note.body));
        content.setCategoryIdentifier(&NSString::from_str(&note.category));
        content.setThreadIdentifier(&NSString::from_str(&note.thread));
        let info = NSString::from_str(&note.info);
        let info = NSDictionary::<NSString, NSString>::from_slices(
            &[&*NSString::from_str(INFO)],
            &[&*info],
        );
        // SAFETY: a dictionary of strings is a valid property-list `userInfo`.
        unsafe { content.setUserInfo(info.cast_unchecked()) };
        if note.time_sensitive {
            content.setInterruptionLevel(UNNotificationInterruptionLevel::TimeSensitive);
        }
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(&note.id),
            &content,
            None,
        );
        let done = RcBlock::new(|error: *mut NSError| {
            // SAFETY: the center hands a live `NSError` (or nil) for the block's duration.
            if let Some(error) = unsafe { error.as_ref() } {
                tracing::debug!(error = %error.localizedDescription(), "alert not shown");
            }
        });
        center.addNotificationRequest_withCompletionHandler(&request, Some(&*done));
    }

    pub(super) fn withdraw(ids: &[String]) {
        let Some(center) = center() else {
            return;
        };
        let ids: Vec<Retained<NSString>> = ids.iter().map(|id| NSString::from_str(id)).collect();
        let ids = NSArray::from_retained_slice(&ids);
        center.removeDeliveredNotificationsWithIdentifiers(&ids);
        center.removePendingNotificationRequestsWithIdentifiers(&ids);
    }
}
