//! Operator app launches: a session the app started, drafted or stopped on a
//! caller's behalf, through the normal durable draft/queue path.
use super::*;

impl Conversation {
    /// App launches are saved before installing a view. Reuse the normal durable
    /// draft/queue path instead of submitting an unrecorded seed prompt.
    pub(crate) fn install_app_launch(
        &mut self,
        settings: Settings,
        send: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.history_ready() {
            self.defer_history_action(
                move |this, cx| this.install_app_launch(settings, send, cx),
                cx,
            );
            return;
        }
        self.install_transfer(settings, None, cx);
        if send {
            self.draft_action(true, cx);
        }
        cx.emit(SessionChanged);
    }

    pub(crate) fn save_app_draft(&mut self, prompt: String, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_text(prompt, cx));
        self.save_message_draft(cx);
    }

    pub(crate) fn stop_app_session(&mut self, cx: &mut Context<Self>) {
        if self.needs_history_restore() {
            self.stop_if_busy(cx);
            self.defer_history_action(|this, cx| this.stop_app_session(cx), cx);
            return;
        }
        self.stop_if_busy(cx);
        self.follow_ups.state.paused = true;
        self.follow_ups.ready = false;
        self.follow_ups.requested = None;
        self.save_follow_ups(cx);
    }
}
