//! The macOS menu-bar status item: the quick composer and Quit. AppKit owns
//! menu navigation and accessibility; actions return to GPUI.
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{
    AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{NSImage, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem};
use objc2_foundation::{NSData, NSSize, NSString};

/// What a status menu item asks the app to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StatusAction {
    QuickComposer,
    Quit,
}
struct Action {
    action: StatusAction,
    run: Rc<dyn Fn(StatusAction)>,
}
define_class!(
    #[unsafe(super(NSMenuItem))]
    #[name="MonocodeStatusMenuItem"]
    #[thread_kind=MainThreadOnly]
    #[ivars=Action]
    struct Item;
    impl Item{
        #[unsafe(method(runStatusAction:))]
        fn run(&self,_:Option<&AnyObject>){(self.ivars().run)(self.ivars().action);}
    }
);
impl Item {
    fn new(
        mtm: MainThreadMarker,
        title: &str,
        action: StatusAction,
        run: Rc<dyn Fn(StatusAction)>,
    ) -> Retained<Self> {
        let this = mtm.alloc().set_ivars(Action { action, run });
        let item: Retained<Self> = unsafe {
            msg_send![super(this),initWithTitle:&*NSString::from_str(title),action:Some(sel!(runStatusAction:)),keyEquivalent:&*NSString::from_str("")]
        };
        // The status menu retains each item for as long as it can be invoked.
        unsafe {
            item.setTarget(Some(&item));
        }
        item
    }
}
pub(crate) struct StatusItem {
    item: Retained<NSStatusItem>,
}
impl StatusItem {
    pub(crate) fn new(run: Rc<dyn Fn(StatusAction)>) -> Option<Self> {
        let mtm = MainThreadMarker::new()?;
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(-1.);
        if let Some(button) = item.button(mtm) {
            let data = NSData::with_bytes(include_bytes!("../assets/app-icon.png"));
            if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
                image.setSize(NSSize::new(18., 18.));
                image.setTemplate(true);
                button.setImage(Some(&image));
            } else {
                button.setTitle(&NSString::from_str("Mono"));
            }
            button.setToolTip(Some(&NSString::from_str("Onicode")));
        }
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);
        menu.addItem(&Item::new(
            mtm,
            "Quick Composer",
            StatusAction::QuickComposer,
            run.clone(),
        ));
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&Item::new(mtm, "Quit Onicode", StatusAction::Quit, run));
        item.setMenu(Some(&menu));
        Some(Self { item })
    }
}
impl Drop for StatusItem {
    fn drop(&mut self) {
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}
