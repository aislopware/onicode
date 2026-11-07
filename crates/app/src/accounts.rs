//! Shared provider accounts, identities and per-account subscription snapshots.
mod masked;
pub(crate) mod resets;
mod signin;
pub(crate) mod ui;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, Focusable, Global, Subscription, Task, Window};
use onicode_protocol::accounts::{Account, AuthAction, Book, Profile};
use onicode_protocol::agents::{
    AccountsEvent, AccountsState, AddAccount, CancelLogin, Catalog, CatalogRead, Login, Logout,
    RefreshSignIn, RefreshUsage, RemoveAccount, RenameAccount, SelectAccount, UsageEntry, UsageKey,
    UsageRead, WatchAccounts,
};
use onicode_protocol::provider::Kind;
use onicode_protocol::usage::{FetchError, Snapshot};
use onicode_protocol::{OpId, Outcome, Rejection, accounts, usage};

use crate::engine_client::{Engine, WatchUpdate, Watching};
use crate::input::{self, Input};

pub(crate) struct Shared(pub Entity<Accounts>);
impl Global for Shared {}
pub(crate) fn get(cx: &App) -> Option<Entity<Accounts>> {
    cx.try_global::<Shared>().map(|s| s.0.clone())
}
/// Starts the accounts center over `engine`'s `WatchAccounts`, once per app.
pub(crate) fn install(engine: Engine, cx: &mut App) {
    if get(cx).is_some() {
        return;
    }
    let center = cx.new(|cx| Accounts::new(engine, cx));
    cx.set_global(Shared(center));
}

/// What a mutating Agents call came to, as the error a view shows.
pub(crate) fn applied(result: Result<Outcome, String>) -> Result<(), String> {
    match result? {
        Outcome::Applied => Ok(()),
        Outcome::Rejected(Rejection::Denied(reason)) => Err(reason),
        Outcome::Rejected(Rejection::Gone) => Err("Provider account no longer exists".into()),
        Outcome::Rejected(Rejection::AlreadyResolved | Rejection::StaleRev) => {
            Err("Another device changed this account first".into())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Key {
    pub kind: Kind,
    pub profile: Profile,
    pub billing: String,
}
impl Key {
    pub(crate) fn new(kind: Kind, profile: Profile, model: Option<&str>) -> Self {
        let billing = if kind == Kind::Pi {
            model
                .unwrap_or("")
                .trim_start_matches("pi:")
                .split('/')
                .next()
                .unwrap_or("")
                .into()
        } else {
            String::new()
        };
        Self {
            kind,
            profile,
            billing,
        }
    }

    /// The key the engine knows this usage by.
    fn usage_key(&self) -> UsageKey {
        UsageKey {
            provider: self.kind,
            account: self.profile.id.clone(),
            billing: self.billing.clone(),
        }
    }
}
#[derive(Clone, Default)]
pub(crate) struct Cached {
    pub snapshot: Option<Snapshot>,
    pub fetching: bool,
    pub error: Option<FetchError>,
    pub attempted: Option<Instant>,
    pub generation: u64,
    pub queued: bool,
}
impl Cached {
    pub(crate) fn complete(&mut self, result: Result<Snapshot, FetchError>) {
        self.fetching = false;
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(error) => {
                if error.unavailable {
                    self.snapshot = None;
                }
                self.error = Some(error);
            }
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Tone {
    Ready,
    Low,
    Exhausted,
    Checking,
    Unknown,
}
pub(crate) struct Status {
    pub tone: Tone,
    pub label: String,
    pub detail: Option<String>,
}
#[derive(Clone)]
pub(crate) enum AccountEvent {
    Updated,
    AuthStarted,
    PickerDismiss,
    Manage,
    Authenticated(Kind, String),
    Removing(Kind, String),
    Select {
        kind: Kind,
        id: String,
        project: String,
        session: Option<i64>,
    },
}
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Surface {
    Settings,
    InlineSettings,
    Footer,
}
struct Editor {
    kind: Kind,
    id: Option<String>,
    input: Entity<Input>,
    surface: Surface,
    previous_focus: Option<gpui::FocusHandle>,
    _submit: Subscription,
    _changed: Subscription,
}
struct SignIn {
    kind: Kind,
    profile: Profile,
    action: AuthAction,
    surface: Surface,
    /// The engine's `Login` attempt id, which `CancelLogin` names.
    attempt: String,
    cancel: Arc<AtomicBool>,
    complete: bool,
    error: Option<String>,
    adding: Option<Account>,
    started: Instant,
}
struct Picker {
    kind: Kind,
    project: String,
    session: Option<i64>,
    index: usize,
}
pub(crate) struct Accounts {
    /// The engine's book as shown: an account being added stays out until its sign-in completes.
    pub book: Book,
    pub usage: HashMap<Key, Cached>,
    pub reset_actions: HashMap<Key, resets::ResetAction>,
    pub identities: HashMap<(Kind, String), accounts::Identity>,
    /// Accounts whose sign-in the engine was asked for since they last changed.
    identity_loading: std::collections::HashSet<(Kind, String)>,
    /// The engine's accounts as its watch last said.
    engine_state: AccountsState,
    engine: Engine,
    _watching: Watching<AccountsState, AccountsEvent>,
    _mirror: Task<()>,
    editor: Option<Editor>,
    signin: Option<SignIn>,
    pub error: Option<String>,
    working: bool,
    pub mask_emails: bool,
    pub show_remaining: bool,
    revealed: std::collections::HashSet<(Kind, String)>,
    dialog_focus: gpui::FocusHandle,
    dialog_presented: bool,
    previous_focus: Option<gpui::FocusHandle>,
    picker_focus: gpui::FocusHandle,
    picker: Option<Picker>,
    _clock: Task<()>,
}
impl EventEmitter<AccountEvent> for Accounts {}
impl Accounts {
    fn new(engine: Engine, cx: &mut Context<Self>) -> Self {
        let watching = engine.watch::<WatchAccounts>(|since| WatchAccounts { since });
        let updates = watching.updates.clone();
        let mirror = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                if this.update(cx, |this, cx| this.absorb(update, cx)).is_err() {
                    break;
                }
            }
        });
        let clock = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                if this
                    .update(cx, |this, cx| {
                        let keys: Vec<_> = this.usage.keys().cloned().collect();
                        for key in keys {
                            this.refresh(key, false, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            book: Book::default(),
            usage: HashMap::new(),
            reset_actions: HashMap::new(),
            identities: HashMap::new(),
            identity_loading: Default::default(),
            engine_state: AccountsState::default(),
            engine,
            _watching: watching,
            _mirror: mirror,
            editor: None,
            signin: None,
            error: None,
            working: false,
            mask_emails: true,
            show_remaining: false,
            revealed: Default::default(),
            dialog_focus: cx.focus_handle(),
            dialog_presented: false,
            previous_focus: None,
            picker_focus: cx.focus_handle(),
            picker: None,
            _clock: clock,
        }
    }

    /// The account `id` as a session names it. Its home is the engine's to set.
    pub(crate) fn profile(&self, id: &str) -> Profile {
        Profile {
            id: id.to_owned(),
            root: None,
        }
    }

    /// Takes in what the engine's accounts watch sent.
    fn absorb(
        &mut self,
        update: WatchUpdate<AccountsState, AccountsEvent>,
        cx: &mut Context<Self>,
    ) {
        match update {
            WatchUpdate::Snapshot(state) => {
                self.engine_state = *state;
                for entry in self.engine_state.sign_ins.clone() {
                    self.sign_in_read(entry.provider, &entry.account, &entry.sign_in.identity);
                }
                for entry in self.engine_state.usage.clone() {
                    self.usage_read(entry);
                }
            }
            WatchUpdate::Events(events) => {
                for event in events {
                    self.engine_state.apply(&event);
                    match event {
                        AccountsEvent::Book(_) => {}
                        AccountsEvent::SignIn(entry) => {
                            let identity = &entry.sign_in.identity;
                            self.sign_in_read(entry.provider, &entry.account, identity);
                        }
                        AccountsEvent::Usage(entry) => self.usage_read(entry),
                    }
                }
            }
        }
        self.show_book();
        cx.emit(AccountEvent::Updated);
        cx.notify();
    }

    /// Takes `state` as if the engine's watch had sent it: a test's accounts.
    #[cfg(test)]
    pub(crate) fn script(&mut self, state: AccountsState, cx: &mut Context<Self>) {
        // The engine's own snapshot, still on its way, would replace the script.
        self._mirror = Task::ready(());
        self.absorb(WatchUpdate::Snapshot(Box::new(state)), cx);
    }

    /// The engine's book, less an account whose first sign-in is still running.
    fn show_book(&mut self) {
        let mut book = self.engine_state.book.clone();
        if let Some(adding) = self
            .signin
            .as_ref()
            .filter(|signin| !signin.complete)
            .and_then(|signin| signin.adding.as_ref())
        {
            book.accounts
                .retain(|account| account.provider != adding.provider || account.id != adding.id);
        }
        self.book = book;
    }

    fn sign_in_read(&mut self, kind: Kind, id: &str, identity: &accounts::Identity) {
        let key = (kind, id.to_owned());
        if identity.email.is_some()
            || identity.name.is_some()
            || identity.plan.is_some()
            || identity.organization.is_some()
        {
            self.identities.insert(key, identity.clone());
        } else {
            self.identities.remove(&key);
        }
    }

    fn usage_read(&mut self, entry: UsageEntry) {
        let key = Key {
            kind: entry.key.provider,
            profile: self.profile(&entry.key.account),
            billing: entry.key.billing,
        };
        if key.profile.id != accounts::DEFAULT
            && !self.engine_state.book.exists(key.kind, &key.profile.id)
        {
            return;
        }
        let state = self.usage.entry(key.clone()).or_default();
        let refreshed = matches!(entry.usage, UsageRead::Snapshot(_));
        match entry.usage {
            UsageRead::Snapshot(snapshot) => state.complete(Ok(snapshot)),
            UsageRead::Failed(error) => state.complete(Err(error)),
            // The account's turns have reported nothing yet: nothing to show, and no error.
            UsageRead::Unreported => {
                state.fetching = false;
                state.snapshot = None;
                state.error = None;
            }
        }
        if refreshed
            && let Some(action) = self.reset_actions.get_mut(&key)
            && !action.applying()
        {
            action.row = None;
        }
        if let Some(identity) = state.snapshot.as_ref().and_then(|s| s.identity.clone()) {
            self.identities
                .insert((key.kind, key.profile.id.clone()), identity);
        }
    }

    pub(crate) fn hide_emails(&mut self) {
        self.revealed.clear();
    }

    pub(crate) fn open_picker(
        &mut self,
        kind: Kind,
        selected: &str,
        project: &str,
        session: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker = Some(Picker {
            kind,
            project: project.into(),
            session,
            index: self
                .book
                .list(kind)
                .iter()
                .position(|a| a.id == selected)
                .unwrap_or_default(),
        });
        window.focus(&self.picker_focus, cx);
        cx.notify();
    }

    fn picker_key(&mut self, event: &gpui::KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = &mut self.picker else {
            return;
        };
        let rows = self.book.list(picker.kind);
        if rows.is_empty() {
            return;
        }
        match event.keystroke.key.as_str() {
            "down" => picker.index = (picker.index + 1) % rows.len(),
            "up" => picker.index = (picker.index + rows.len() - 1) % rows.len(),
            "home" => picker.index = 0,
            "end" => picker.index = rows.len() - 1,
            "enter" | "space" => {
                let (kind, project, session) =
                    (picker.kind, picker.project.clone(), picker.session);
                let id = rows[picker.index.min(rows.len() - 1)].id.clone();
                self.choose(kind, id, project, session, cx);
            }
            "escape" | "tab" => cx.emit(AccountEvent::PickerDismiss),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// An account to switch to from `selected`, its usage read at `now` (Unix ms).
    pub(crate) fn suggestion(&self, kind: Kind, selected: &str, now: i64) -> Option<Account> {
        let snapshot = |id: &str| {
            self.usage
                .get(&Key::new(kind, self.profile(id), None))?
                .snapshot
                .as_ref()
        };
        if snapshot(selected)?.standing(now)? == usage::Standing::Ready {
            return None;
        }
        // Ready accounts with the most room first; one that reported only its status ranks last.
        self.book
            .list(kind)
            .into_iter()
            .filter(|a| a.id != selected)
            .filter_map(|a| {
                let theirs = snapshot(&a.id)?;
                (theirs.standing(now)? == usage::Standing::Ready)
                    .then(|| (a, theirs.headroom(now).unwrap_or(0.)))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(a, _)| a)
    }

    pub(crate) fn selected(&self, kind: Kind, project: &str) -> Profile {
        self.profile(&self.book.selected(kind, project))
    }

    /// React's `accountStatus`: a tone, a word, and an optional detail, its
    /// usage read at `now` (Unix ms).
    pub(crate) fn status(&self, kind: Kind, id: &str, now: i64) -> Status {
        let key = Key::new(kind, self.profile(id), None);
        // Claude reports nothing until the account's next turn: not known yet, not a failure.
        let checking = Status {
            tone: Tone::Checking,
            label: if usage::from_turns(kind) {
                "Ready"
            } else {
                "Checking…"
            }
            .into(),
            detail: None,
        };
        let Some(state) = self.usage.get(&key) else {
            return checking;
        };
        if let Some(standing) = state.snapshot.as_ref().and_then(|s| s.standing(now)) {
            return match standing {
                usage::Standing::Exhausted(back) => Status {
                    tone: Tone::Exhausted,
                    label: "Exhausted".into(),
                    detail: back.map(|at| format!("back in {}", usage::duration(at - now))),
                },
                usage::Standing::Low(left) => Status {
                    tone: Tone::Low,
                    label: "Running low".into(),
                    detail: left.map(|left| format!("{left:.0}% left")),
                },
                usage::Standing::Ready => Status {
                    tone: Tone::Ready,
                    label: "Ready".into(),
                    detail: None,
                },
            };
        }
        if state.fetching || (state.snapshot.is_none() && state.error.is_none()) {
            return checking;
        }
        Status {
            tone: Tone::Unknown,
            label: state
                .error
                .as_ref()
                .map(|e| e.message.clone())
                .unwrap_or_else(|| "Usage unavailable".into()),
            detail: None,
        }
    }

    /// Asks the engine to read `key`'s usage again: what the account's turns reported, or the
    /// provider's poll at most every 15 minutes (5 once a window has reset), unless `force`d.
    /// What it read arrives on the accounts watch.
    pub(crate) fn refresh(&mut self, key: Key, force: bool, cx: &mut Context<Self>) {
        if cfg!(test) {
            return;
        }
        let state = self.usage.entry(key.clone()).or_default();
        if !usage::from_turns(key.kind) {
            if state.fetching {
                state.queued |= force;
                return;
            }
            let reset = state.snapshot.as_ref().is_some_and(|s| {
                s.windows.iter().any(|w| {
                    w.resets_at
                        .is_some_and(|at| at > s.updated_at && at <= usage::now_ms())
                })
            });
            if !force
                && state.attempted.is_some_and(|at| {
                    at.elapsed() < Duration::from_secs(if reset { 300 } else { 900 })
                })
            {
                return;
            }
            state.fetching = true;
            state.attempted = Some(Instant::now());
        }
        let generation = state.generation;
        let call = self.engine.call::<RefreshUsage>(
            None,
            RefreshUsage {
                key: key.usage_key(),
                force: true,
            },
        );
        cx.spawn(async move |this, cx| {
            let result = applied(call.await);
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.usage.get_mut(&key) else {
                    return;
                };
                if state.generation != generation {
                    return;
                }
                state.fetching = false;
                let queued = std::mem::take(&mut state.queued);
                if let Err(message) = result {
                    state.complete(Err(FetchError {
                        message,
                        unavailable: false,
                    }));
                }
                if queued {
                    this.refresh(key, true, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn load_provider(&mut self, kind: Kind, force: bool, cx: &mut Context<Self>) {
        for account in self.book.list(kind) {
            self.refresh(Key::new(kind, self.profile(&account.id), None), force, cx);
            if force {
                self.identities.remove(&(kind, account.id.clone()));
                self.identity_loading.remove(&(kind, account.id.clone()));
            }
            self.identity(kind, &account.id, cx);
        }
    }

    /// Asks the engine who `id` is signed in as, once until the account changes; the answer
    /// arrives on the accounts watch.
    fn identity(&mut self, kind: Kind, id: &str, cx: &mut Context<Self>) {
        let key = (kind, id.to_owned());
        if cfg!(test) || self.identities.contains_key(&key) || !self.identity_loading.insert(key) {
            return;
        }
        let call = self.engine.call::<RefreshSignIn>(
            None,
            RefreshSignIn {
                provider: kind,
                account: id.to_owned(),
            },
        );
        cx.background_executor().spawn(call).detach();
    }

    /// Runs a mutating call; once the engine applied it, `book` shows until its watch says the
    /// same, and on failure the error shows.
    fn commit(
        &mut self,
        book: Book,
        call: impl std::future::Future<Output = Result<Outcome, String>> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.working = true;
        if let Some(editor) = &self.editor {
            editor
                .input
                .update(cx, |input, _| input.set_read_only(true));
        }
        self.error = None;
        cx.spawn(async move |this, cx| {
            let result = applied(call.await);
            let _ = this.update(cx, |this, cx| {
                this.working = false;
                if let Some(editor) = &this.editor {
                    editor
                        .input
                        .update(cx, |input, _| input.set_read_only(false));
                }
                match result {
                    Ok(()) => {
                        this.book = book;
                        this.editor = None;
                        cx.emit(AccountEvent::Updated);
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn choose(
        &mut self,
        kind: Kind,
        id: String,
        project: String,
        session: Option<i64>,
        cx: &mut Context<Self>,
    ) {
        if self.working || !self.book.exists(kind, &id) {
            return;
        }
        let mut book = self.book.clone();
        if let Err(error) = book.select(kind, &project, &id) {
            self.error = Some(error);
            return;
        }
        let call = self.engine.call::<SelectAccount>(
            Some(OpId::new()),
            SelectAccount {
                provider: kind,
                project: project.clone(),
                account: id.clone(),
            },
        );
        self.commit(book, call, cx);
        cx.emit(AccountEvent::Select {
            kind,
            id,
            project,
            session,
        });
    }

    pub(crate) fn start_edit(
        &mut self,
        kind: Kind,
        id: Option<String>,
        surface: Surface,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.working {
            return;
        }
        let value = id
            .as_ref()
            .and_then(|id| self.book.list(kind).into_iter().find(|a| a.id == *id))
            .map(|a| a.label)
            .unwrap_or_default();
        let input = cx.new(|cx| {
            let mut input = Input::field(
                format!(
                    "{} {} account",
                    if id.is_some() { "Rename" } else { "New" },
                    kind.label()
                ),
                "Work or Personal",
                crate::theme::content(0.25),
                cx,
            );
            input.set_font_size(12.);
            input.set_max_chars(48);
            input
        });
        input.update(cx, |input, cx| input.set_text(value, cx));
        let submit = cx.subscribe_in(&input, window, |this, _, _: &input::Submitted, _, cx| {
            this.submit_editor(cx)
        });
        let changed = cx.subscribe(&input, |_, _, _: &input::Changed, cx| cx.notify());
        let previous_focus = window.focused(cx);
        window.focus(&input.read(cx).focus_handle(cx), cx);
        self.editor = Some(Editor {
            kind,
            id,
            input,
            surface,
            previous_focus,
            _submit: submit,
            _changed: changed,
        });
        self.error = None;
        cx.notify();
    }

    fn submit_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        if self.working {
            return;
        }
        let label = editor.input.read(cx).content.to_string();
        if label.trim().is_empty() {
            return;
        }
        let kind = editor.kind;
        let id = editor.id.clone();
        let mut book = self.book.clone();
        if let Some(id) = id {
            match book.rename(kind, &id, &label) {
                Ok(()) => {
                    let call = self.engine.call::<RenameAccount>(
                        Some(OpId::new()),
                        RenameAccount {
                            provider: kind,
                            account: id,
                            label,
                        },
                    );
                    self.commit(book, call, cx);
                }
                Err(error) => {
                    self.error = Some(error);
                    cx.notify();
                }
            }
            return;
        }
        if let Err(error) = book.add(kind, &label) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        // The engine keeps the account from here; it shows once its sign-in completes.
        self.working = true;
        editor
            .input
            .update(cx, |input, _| input.set_read_only(true));
        self.error = None;
        let call = self.engine.call::<AddAccount>(
            Some(OpId::new()),
            AddAccount {
                provider: kind,
                label,
            },
        );
        cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                this.working = false;
                if let Some(editor) = &this.editor {
                    editor
                        .input
                        .update(cx, |input, _| input.set_read_only(false));
                }
                match result {
                    Ok(account) => this.begin_auth(
                        kind,
                        &account.id.clone(),
                        AuthAction::SignIn,
                        Some(account),
                        cx,
                    ),
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn begin_auth(
        &mut self,
        kind: Kind,
        id: &str,
        action: AuthAction,
        adding: Option<Account>,
        cx: &mut Context<Self>,
    ) {
        let surface = self
            .editor
            .as_ref()
            .filter(|_| adding.is_some())
            .map_or(Surface::Settings, |editor| editor.surface);
        self.start_auth(kind, id, action, adding, surface, cx);
    }

    pub(crate) fn begin_footer_auth(&mut self, kind: Kind, id: &str, cx: &mut Context<Self>) {
        self.start_auth(kind, id, AuthAction::SignIn, None, Surface::Footer, cx);
    }

    fn start_auth(
        &mut self,
        kind: Kind,
        id: &str,
        action: AuthAction,
        adding: Option<Account>,
        surface: Surface,
        cx: &mut Context<Self>,
    ) {
        if self.working
            || self
                .signin
                .as_ref()
                .is_some_and(|s| !s.complete && s.error.is_none())
        {
            return;
        }
        let surface = if surface == Surface::Settings && adding.is_some() {
            Surface::InlineSettings
        } else {
            surface
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let request = cancel.clone();
        let attempt = onicode_protocol::new_id();
        if surface != Surface::Footer || adding.is_some() {
            self.working = true;
        }
        self.error = None;
        let profile = self.profile(id);
        if self.signin.is_none() {
            self.dialog_presented = false;
        }
        if action == AuthAction::SignOut {
            cx.emit(AccountEvent::Removing(kind, id.into()));
        }
        self.signin = Some(SignIn {
            kind,
            profile: profile.clone(),
            action,
            surface,
            attempt: attempt.clone(),
            cancel,
            complete: false,
            error: None,
            adding,
            started: Instant::now(),
        });
        if self.has_modal_auth() {
            cx.emit(AccountEvent::AuthStarted);
        }
        if let Some(editor) = &self.editor {
            editor
                .input
                .update(cx, |input, _| input.set_read_only(true));
        }
        self.show_book();
        let engine = self.engine.clone();
        let account = profile.id;
        let work = cx.background_executor().spawn(async move {
            let result = match action {
                AuthAction::SignIn => {
                    engine
                        .call::<Login>(
                            Some(OpId::new()),
                            Login {
                                provider: kind,
                                account: account.clone(),
                                attempt,
                            },
                        )
                        .await
                }
                AuthAction::SignOut => {
                    engine
                        .call::<Logout>(
                            Some(OpId::new()),
                            Logout {
                                provider: kind,
                                account: account.clone(),
                            },
                        )
                        .await
                }
            };
            // A sign-in or out changes the models the account is offered.
            let _ = engine
                .call::<Catalog>(
                    None,
                    Catalog {
                        provider: kind,
                        account,
                        read: CatalogRead::Reload,
                    },
                )
                .await;
            applied(result)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                let Some(signin) = &mut this.signin else {
                    return;
                };
                let kind = signin.kind;
                let id = signin.profile.id.clone();
                if !Arc::ptr_eq(&signin.cancel, &request) || signin.cancel.load(Ordering::Relaxed) {
                    return;
                }
                if signin.surface != Surface::Footer || signin.adding.is_some() {
                    this.working = false;
                }
                if let Some(editor) = &this.editor {
                    editor
                        .input
                        .update(cx, |input, _| input.set_read_only(false));
                }
                match result {
                    Err(error) => {
                        signin.error = Some(error.clone());
                        if signin.adding.is_some() {
                            this.error = Some(error);
                        }
                    }
                    Ok(()) => {
                        signin.complete = true;
                        signin.adding = None;
                        this.show_book();
                        this.invalidate(kind, &id, cx);
                        cx.emit(AccountEvent::Authenticated(kind, id));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn invalidate(&mut self, kind: Kind, id: &str, cx: &mut Context<Self>) {
        self.reset_actions
            .retain(|key, _| key.kind != kind || key.profile.id != id);
        self.identities.remove(&(kind, id.to_owned()));
        self.identity_loading.remove(&(kind, id.to_owned()));
        for (key, state) in &mut self.usage {
            if key.kind == kind && key.profile.id == id {
                state.generation += 1;
                state.fetching = false;
                state.snapshot = None;
                state.attempted = None;
            }
        }
        self.refresh(Key::new(kind, self.profile(id), None), true, cx);
        self.identity(kind, id, cx);
    }

    fn remove(&mut self, account: Account, window: &mut Window, cx: &mut Context<Self>) {
        if self.working || account.is_default() {
            return;
        }
        let answer=window.prompt(gpui::PromptLevel::Warning,&format!("Remove “{}”?",account.label),Some("Its stored credentials will be deleted and any running turns for this account will stop. Existing conversations stay in history, but cannot continue until you switch accounts."),&["Remove account","Cancel"],cx);
        cx.spawn_in(window, async move |this, cx| {
            if !matches!(answer.await, Ok(0)) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let kind = Kind::from_id(&account.provider);
                let mut book = this.book.clone();
                if book.remove(kind, &account.id).is_err() {
                    return;
                }
                this.working = true;
                cx.emit(AccountEvent::Removing(kind, account.id.clone()));
                // The engine stops the account's agents, signs it out through its CLI and drops
                // it from the book.
                let call = this.engine.call::<RemoveAccount>(
                    Some(OpId::new()),
                    RemoveAccount {
                        provider: kind,
                        account: account.id.clone(),
                    },
                );
                cx.spawn(async move |this, cx| {
                    let result = applied(call.await);
                    let _ = this.update(cx, |this, cx| {
                        this.working = false;
                        match result {
                            Ok(()) => {
                                this.book = book;
                                this.usage.retain(|key, _| {
                                    key.kind != kind || key.profile.id != account.id
                                });
                                this.reset_actions.retain(|key, _| {
                                    key.kind != kind || key.profile.id != account.id
                                });
                                this.identities.remove(&(kind, account.id));
                                cx.emit(AccountEvent::Updated);
                            }
                            Err(error) => this.error = Some(error),
                        }
                        cx.notify();
                    });
                })
                .detach();
                cx.notify();
            });
        })
        .detach();
    }
}
impl Accounts {
    /// Onicode came back to the front while a browser sign-in is still
    /// waiting. A finished login reaches the CLI within a second or two, so if
    /// it is still waiting after a short grace the browser was closed or
    /// abandoned: reset to Sign in instead of waiting out the CLI's 10 minutes.
    pub(crate) fn app_reactivated(&mut self, cx: &mut Context<Self>) {
        let Some(signin) = &self.signin else {
            return;
        };
        if signin.complete
            || signin.error.is_some()
            || signin.action != AuthAction::SignIn
            || signin.started.elapsed() < Duration::from_secs(2)
        {
            return;
        }
        let attempt = signin.cancel.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(5)).await;
            let _ = this.update(cx, |this, cx| {
                let waiting = this.signin.as_ref().is_some_and(|s| {
                    Arc::ptr_eq(&s.cancel, &attempt) && !s.complete && s.error.is_none()
                });
                if waiting {
                    this.cancel_auth(cx);
                }
            });
        })
        .detach();
    }
}
impl Accounts {
    /// Stops an unfinished sign-in's CLI and lets go of the account it was adding.
    fn abandon(&mut self, signin: &SignIn, cx: &mut Context<Self>) {
        let engine = self.engine.clone();
        let attempt = signin.attempt.clone();
        let adding = signin.adding.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = engine
                    .call::<CancelLogin>(Some(OpId::new()), CancelLogin { attempt })
                    .await;
                if let Some(account) = adding {
                    let _ = engine
                        .call::<RemoveAccount>(
                            Some(OpId::new()),
                            RemoveAccount {
                                provider: Kind::from_id(&account.provider),
                                account: account.id,
                            },
                        )
                        .await;
                }
            })
            .detach();
        self.show_book();
        if let Some(account) = &signin.adding {
            self.book
                .accounts
                .retain(|a| a.provider != account.provider || a.id != account.id);
        }
    }
}
impl Drop for Accounts {
    fn drop(&mut self) {
        if let Some(signin) = &self.signin {
            signin.cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_cache_preserves_stale_data_on_errors_and_separates_accounts_and_pi_billing() {
        let snapshot = Snapshot {
            windows: vec![usage::Window {
                used_percent: Some(40.),
                minutes: 300,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut state = Cached::default();
        state.complete(Ok(snapshot));
        state.complete(Err(FetchError {
            message: "Offline".into(),
            unavailable: false,
        }));
        assert_eq!(
            state.snapshot.as_ref().unwrap().windows[0].used_percent,
            Some(40.)
        );
        state.complete(Err(FetchError {
            message: "Not signed in".into(),
            unavailable: true,
        }));
        assert!(state.snapshot.is_none());
        assert_ne!(
            Key::new(
                Kind::Codex,
                Profile {
                    id: "a".into(),
                    root: None
                },
                None
            ),
            Key::new(
                Kind::Codex,
                Profile {
                    id: "b".into(),
                    root: None
                },
                None
            )
        );
        assert_eq!(
            Key::new(Kind::Pi, Profile::default(), Some("anthropic/model-a")),
            Key::new(Kind::Pi, Profile::default(), Some("pi:anthropic/model-b"))
        );
        assert_ne!(
            Key::new(Kind::Pi, Profile::default(), Some("anthropic/model")),
            Key::new(Kind::Pi, Profile::default(), Some("openai-codex/model"))
        );
    }
}
