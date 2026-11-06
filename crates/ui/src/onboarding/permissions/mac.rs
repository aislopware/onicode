//! The checklist's Mac: the app's rows through `onicode_grants::access`, the helper's by running
//! Onicode Desktop.app, the login item through `SMAppService`, and the drag panel.

use std::path::PathBuf;
use std::sync::Mutex;

use onicode_grants::guide::{End, Phase};
use onicode_grants::{
    Holder, Install, Privilege, Read, Status, access, agent, bundle, guide, probe_helper,
    request_in_helper,
};

use super::{Platform, ReadPlan};

/// The released app's bundle id, for a process that reads none.
const RELEASE_ID: &str = "dev.aislopware.onicode";

pub(crate) struct Mac {
    /// The app bundle this process runs from; `None` for a bare development binary.
    app: Option<PathBuf>,
    /// Onicode Desktop.app inside it.
    helper: Option<PathBuf>,
    /// The login item's plist name.
    plist: String,
    /// The row the drag panel was last opened for.
    guiding: Mutex<Option<Privilege>>,
}

impl Mac {
    pub(crate) fn new() -> Self {
        let app = std::env::current_exe().ok().and_then(|exe| {
            exe.ancestors()
                .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
                .map(PathBuf::from)
        });
        let id = objc2_foundation::NSBundle::mainBundle()
            .bundleIdentifier()
            .map_or_else(|| RELEASE_ID.to_owned(), |id| id.to_string());
        Self {
            app,
            helper: crate::desktop::local_helper_bundle(),
            plist: bundle::agent_plist(&id),
            guiding: Mutex::new(None),
        }
    }

    fn guiding(&self) -> std::sync::MutexGuard<'_, Option<Privilege>> {
        self.guiding
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The bundle the person drags for `privilege`'s list.
    fn bundle_for(&self, privilege: Privilege) -> Option<&PathBuf> {
        match privilege.holder() {
            Holder::App => self.app.as_ref(),
            Holder::Helper => self.helper.as_ref(),
        }
    }
}

impl Platform for Mac {
    fn install(&self) -> Install {
        self.app
            .as_deref()
            .map_or(Install::Settled, onicode_grants::install_of)
    }

    fn read(&self, plan: &ReadPlan) -> Vec<(Privilege, Status)> {
        let mut reads = Vec::new();
        for privilege in Privilege::ALL {
            let status = match (privilege, privilege.read()) {
                (_, Read::Helper | Read::Confirmed) => continue,
                (Privilege::BackgroundActivity, _) if self.helper.is_none() => Status::Unavailable,
                (Privilege::BackgroundActivity, _) => agent::status(&self.plist).checklist(),
                _ => access::read(privilege, plan.asked.contains(&privilege)),
            };
            reads.push((privilege, status));
        }
        match &self.helper {
            None => {
                reads.extend(Privilege::HELPER.map(|privilege| (privilege, Status::Unavailable)))
            }
            Some(helper) if plan.helper => match probe_helper(helper) {
                Ok(grants) => reads.extend(Privilege::HELPER.map(|privilege| {
                    (
                        privilege,
                        Status::from_bool(grants.get(privilege).unwrap_or(false)),
                    )
                })),
                Err(error) => tracing::debug!(%error, "Onicode Desktop did not answer its probe"),
            },
            Some(_) => {}
        }
        reads
    }

    fn allow(&self, privilege: Privilege) {
        match privilege.holder() {
            Holder::Helper if privilege == Privilege::BackgroundActivity => {
                match agent::register(&self.plist) {
                    Ok(agent::Status::RequiresApproval) => agent::open_login_items(),
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "Onicode Desktop was not registered"),
                }
            }
            Holder::Helper => {
                let Some(helper) = &self.helper else {
                    return;
                };
                if let Err(error) = request_in_helper(helper, privilege) {
                    tracing::warn!(%error, ?privilege, "Onicode Desktop did not ask");
                }
            }
            Holder::App => access::ask(privilege),
        }
    }

    fn open_settings(&self, privilege: Privilege) {
        if privilege.takes_drag()
            && let Some(bundle) = self.bundle_for(privilege)
        {
            match guide::show(privilege, bundle) {
                Ok(()) => {
                    *self.guiding() = Some(privilege);
                    return;
                }
                Err(error) => tracing::warn!(%error, "the drag panel did not open"),
            }
        }
        if !onicode_grants::open_settings(privilege) {
            tracing::info!(?privilege, "System Settings did not open at the list");
            return;
        }
        if let Some(mtm) = objc2::MainThreadMarker::new() {
            let _front = onicode_grants::activate_settings(mtm);
        }
    }

    fn report(&self, privilege: Privilege, granted: bool) {
        if guide::is_open() && guide::privilege() == Some(privilege) {
            guide::report(granted);
        }
    }

    fn guide_granted(&self) -> Option<Privilege> {
        let mut guiding = self.guiding();
        match guide::phase() {
            Some(Phase::Ended(End::Granted)) => guiding.take(),
            Some(Phase::Ended(_)) | None => {
                *guiding = None;
                None
            }
            Some(_) => None,
        }
    }

    fn quit_and_reopen(&self) -> bool {
        let Some(app) = &self.app else {
            return false;
        };
        match onicode_grants::reopen_after_quit(app) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "Onicode could not arrange to reopen");
                false
            }
        }
    }
}
