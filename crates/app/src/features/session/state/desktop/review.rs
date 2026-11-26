//! Gentle Desktop's review mode (the receipt-driven development switch) in
//! one tab.
//!
//! When the backend's `capabilities` advertise `get_review_mode` and
//! `set_review_mode`, this slice reads the switch once the probe lands and
//! keeps the server's answer as the truth: the chip shows it, a
//! `review_mode_changed` event moves it, and a gap or the chip opening reads
//! it again (a change made in the TUI emits nothing). A change takes two
//! calls: the first answers a confirmation the human must see; only a resend
//! with its token writes. A stale token is answered with a fresh
//! confirmation, which simply replaces the one shown.
//!
//! It is pure state on the shared [`DesktopSession`], one request at a time,
//! like the other slices. A Shell without the methods (4.8.0) or without
//! `gentle-ai` never shows the control, and nothing shells out instead.

use pi_rpc::desktop_rpc::{
    Capabilities, DesktopErrorCode, DesktopEvent, DesktopMethod, DesktopRecord, DesktopResponse,
    DesktopRpcError, DesktopSession, ReviewMode, ReviewScope, SetReviewModeOutcome,
};

use super::Send;

/// The wire name of the only event this slice applies.
pub(in crate::features::session) const REVIEW_MODE_CHANGED: &str = "review_mode_changed";

/// What one decision produced: the requests to send, and a failure the tab
/// shows through its report path.
#[derive(Debug, Default)]
pub(in crate::features::session) struct Applied {
    pub(in crate::features::session) sends: Vec<Send>,
    pub(in crate::features::session) report: Option<Report>,
}

/// Why a review-mode change did not happen, for the conversation. A read
/// that finds no review owner is not here: the control just stays hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::features::session) enum Report {
    /// `review_unavailable` on a change: the server's own path-free message.
    Unavailable(String),
    /// `review_mode_busy`: another change is running; nothing was written.
    Busy,
    /// The request timed out before the server answered.
    Timeout,
    /// A transport, server or decoding failure.
    Failure(DesktopRpcError),
    /// A gap in the event sequence: the record was already accepted, so the
    /// wiring resyncs instead of accepting it again.
    OutOfOrderSequence,
}

impl Report {
    /// What the conversation says about it.
    pub(in crate::features::session) fn message(&self) -> String {
        match self {
            Self::Unavailable(message) => {
                format!("Review mode could not be changed: {message}")
            }
            Self::Busy => {
                "Review mode is already being changed; try again in a moment.".to_string()
            }
            Self::Timeout => "The review mode request timed out.".to_string(),
            Self::Failure(error) => format!("The review mode request failed ({error})."),
            Self::OutOfOrderSequence => {
                "Gentle Desktop events were lost; re-reading the review mode.".to_string()
            }
        }
    }
}

/// The dialog Gentle Shell asked Desktop to show before a change, exactly as
/// sent. The token only leaves the slice in the resend the human approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::features::session) struct Confirmation {
    pub(in crate::features::session) enabled: bool,
    pub(in crate::features::session) scope: ReviewScope,
    pub(in crate::features::session) title: String,
    pub(in crate::features::session) message: String,
    token: String,
}

/// One `set_review_mode` the user asked for, waiting for the shared slot.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    enabled: bool,
    scope: ReviewScope,
    token: Option<String>,
}

#[derive(Debug, Default)]
pub(in crate::features::session) struct Review {
    capabilities: Option<Capabilities>,
    /// The switch as the server last reported it.
    mode: Option<ReviewMode>,
    /// The server has no review owner here (`review_unavailable` on a read,
    /// or `unsupported_method`): the control stays hidden.
    unavailable: bool,
    /// A `get_review_mode` is owed: the probe landed, a gap, the chip opened,
    /// or a change whose outcome is unknown.
    read_due: bool,
    /// The change waiting for the slot.
    queued: Option<Change>,
    /// A change is on the wire.
    changing: bool,
    /// The dialog to show; a newer confirmation replaces it.
    confirmation: Option<Confirmation>,
    /// Bumped per request, so an old timer cannot drop a newer one.
    generation: u64,
}

impl Review {
    /// Adopt the peer's capabilities once the probe completes; the first
    /// read is owed from here.
    pub(in crate::features::session) fn set_capabilities(&mut self, capabilities: Capabilities) {
        self.capabilities = Some(capabilities);
        self.read_due = self.supported();
    }

    /// Whether the peer offers both review methods. A switch that can only
    /// be read is not a control.
    pub(in crate::features::session) fn supported(&self) -> bool {
        self.capabilities.as_ref().is_some_and(|capabilities| {
            capabilities.supports(DesktopMethod::GetReviewMode)
                && capabilities.supports(DesktopMethod::SetReviewMode)
        })
    }

    /// The switch to show, once read; `None` hides the control.
    pub(in crate::features::session) fn mode(&self) -> Option<&ReviewMode> {
        self.mode
            .as_ref()
            .filter(|_| self.supported() && !self.unavailable)
    }

    /// Whether a change is queued or on the wire.
    pub(in crate::features::session) fn changing(&self) -> bool {
        self.changing || self.queued.is_some()
    }

    /// The confirmation to show, if the server asked for one.
    pub(in crate::features::session) fn confirmation(&self) -> Option<&Confirmation> {
        self.confirmation.as_ref()
    }

    /// Read the switch again (the chip opened: a TUI change emits nothing).
    pub(in crate::features::session) fn refresh(&mut self) {
        if self.supported() && !self.unavailable {
            self.read_due = true;
        }
    }

    /// Events were lost: a `review_mode_changed` may be among them.
    pub(in crate::features::session) fn rearm(&mut self) {
        self.refresh();
    }

    /// Ask to turn reviews on or off for `scope`. Nothing is written by this
    /// request: the server answers the confirmation to show. `false` when
    /// the peer does not offer the switch.
    pub(in crate::features::session) fn request(
        &mut self,
        enabled: bool,
        scope: ReviewScope,
    ) -> bool {
        if self.mode().is_none() {
            return false;
        }
        self.confirmation = None;
        self.queued = Some(Change {
            enabled,
            scope,
            token: None,
        });
        true
    }

    /// The human approved the dialog: resend its request with its token.
    pub(in crate::features::session) fn confirm(&mut self) -> bool {
        let Some(confirmation) = self.confirmation.take() else {
            return false;
        };
        self.queued = Some(Change {
            enabled: confirmation.enabled,
            scope: confirmation.scope,
            token: Some(confirmation.token),
        });
        true
    }

    /// The human declined: nothing is resent.
    pub(in crate::features::session) fn dismiss(&mut self) {
        self.confirmation = None;
    }

    /// Send what is owed once the slot is free: the user's change first,
    /// then a read.
    pub(in crate::features::session) fn pump(&mut self, session: &mut DesktopSession) -> Applied {
        if session.pending().is_some() || !self.supported() {
            return Applied::default();
        }
        let request = if let Some(change) = self.queued.take() {
            self.changing = true;
            session.issue_set_review_mode(change.enabled, change.scope, change.token.as_deref())
        } else if self.read_due && !self.unavailable {
            self.read_due = false;
            session.issue(DesktopMethod::GetReviewMode)
        } else {
            return Applied::default();
        };
        match request {
            Ok(request) => {
                self.generation += 1;
                Applied {
                    sends: vec![Send {
                        request,
                        generation: self.generation,
                    }],
                    report: None,
                }
            }
            Err(error) => {
                self.changing = false;
                report(Report::Failure(error))
            }
        }
    }

    /// Pi answered the outer `prompt` of this slice's request.
    pub(in crate::features::session) fn outer(
        &mut self,
        session: &mut DesktopSession,
        generation: u64,
        handled: bool,
    ) -> Applied {
        if handled || generation != self.generation || !owns_pending(session) {
            return Applied::default();
        }
        self.rollback(session);
        report(Report::Failure(DesktopRpcError::UnexpectedDisposition {
            disposition: None,
        }))
    }

    /// The pending timeout for `generation` fired.
    pub(in crate::features::session) fn expire(
        &mut self,
        session: &mut DesktopSession,
        generation: u64,
    ) -> Applied {
        if generation != self.generation || !owns_pending(session) {
            return Applied::default();
        }
        self.rollback(session);
        report(Report::Timeout)
    }

    /// [`Self::apply_decoded`] for the raw request.
    #[cfg(test)]
    pub(in crate::features::session) fn apply(
        &mut self,
        session: &mut DesktopSession,
        request: &pi_rpc::UiRequest,
    ) -> Applied {
        self.apply_decoded(session, pi_rpc::desktop_rpc::decode_widget(request))
    }

    /// One incoming UI request; applies only the records this slice owns
    /// (its two answers and `review_mode_changed`).
    /// `decoded` is the request's [`pi_rpc::desktop_rpc::decode_widget`]
    /// result, so the record is not decoded again.
    pub(in crate::features::session) fn apply_decoded(
        &mut self,
        session: &mut DesktopSession,
        decoded: Result<Option<DesktopRecord>, DesktopRpcError>,
    ) -> Applied {
        // The session clears the pending request while correlating, so the
        // method it answered is read first.
        let awaiting = session.pending().map(|(_, method)| method);
        match session.accept_decoded(decoded) {
            Ok(Some(DesktopRecord::Response(response))) => match awaiting {
                Some(DesktopMethod::GetReviewMode) => self.apply_read(response),
                Some(DesktopMethod::SetReviewMode) => self.apply_change(response),
                _ => Applied::default(),
            },
            Ok(Some(DesktopRecord::Event(event))) => self.apply_event(event),
            Err(DesktopRpcError::OutOfOrderSequence { .. }) => {
                self.rearm();
                report(Report::OutOfOrderSequence)
            }
            // Not this protocol's widget, or a late, repeated or malformed
            // record: never a change of the switch.
            _ => Applied::default(),
        }
    }

    /// The backend was replaced or exited: nothing it said applies, and the
    /// next probe reads again.
    pub(in crate::features::session) fn reset(&mut self) {
        *self = Self {
            generation: self.generation + 1,
            ..Self::default()
        };
    }

    /// Drop the request in flight. Whether a change landed is unknown, so
    /// the switch is read again.
    fn rollback(&mut self, session: &mut DesktopSession) {
        session.abandon();
        self.generation += 1;
        if std::mem::take(&mut self.changing) {
            self.read_due = true;
        }
    }

    fn apply_read(&mut self, response: DesktopResponse) -> Applied {
        match response.review_mode() {
            Ok(mode) => {
                self.mode = Some(mode);
                Applied::default()
            }
            // No review owner (`gentle-ai` missing) or no method after all:
            // the control is hidden, which is not a failure of this tab.
            Err(DesktopRpcError::RemoteError(info))
                if matches!(
                    info.kind(),
                    Some(DesktopErrorCode::ReviewUnavailable | DesktopErrorCode::UnsupportedMethod)
                ) =>
            {
                self.unavailable = true;
                self.mode = None;
                Applied::default()
            }
            Err(error) => report(Report::Failure(error)),
        }
    }

    fn apply_change(&mut self, response: DesktopResponse) -> Applied {
        self.changing = false;
        match response.set_review_mode() {
            Ok(result) => {
                self.mode = Some(result.mode);
                if let SetReviewModeOutcome::ConfirmationRequired(confirmation) = result.outcome {
                    // A first answer, or a stale token asked again: either
                    // way the newest text and token are what the human sees.
                    self.confirmation = Some(Confirmation {
                        enabled: result.enabled,
                        scope: result.scope,
                        title: confirmation.title,
                        message: confirmation.message,
                        token: confirmation.token,
                    });
                }
                Applied::default()
            }
            Err(DesktopRpcError::RemoteError(info)) => match info.kind() {
                Some(DesktopErrorCode::ReviewModeBusy) => report(Report::Busy),
                // A write whose outcome is unknown is never retried: the
                // switch is read again instead.
                Some(DesktopErrorCode::ReviewUnavailable) => {
                    self.read_due = true;
                    report(Report::Unavailable(info.message))
                }
                _ => report(Report::Failure(DesktopRpcError::RemoteError(info))),
            },
            Err(error) => report(Report::Failure(error)),
        }
    }

    /// A `review_mode_changed` event, applied only when the peer advertised
    /// that exact name.
    fn apply_event(&mut self, event: DesktopEvent) -> Applied {
        let advertised = self
            .capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.emits(REVIEW_MODE_CHANGED));
        if event.event != REVIEW_MODE_CHANGED || !advertised {
            return Applied::default();
        }
        match event.review_mode_changed() {
            Ok(changed) => {
                self.mode = Some(changed.mode);
                Applied::default()
            }
            Err(error) => report(Report::Failure(error)),
        }
    }
}

fn report(report: Report) -> Applied {
    Applied {
        sends: Vec::new(),
        report: Some(report),
    }
}

/// Whether the pending request is this slice's: the other slices share the
/// session, so a stale review timer must not drop theirs.
fn owns_pending(session: &DesktopSession) -> bool {
    matches!(
        session.pending().map(|(_, method)| method),
        Some(DesktopMethod::GetReviewMode | DesktopMethod::SetReviewMode)
    )
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{
        Capabilities, DesktopSession, PROMPT_COMMAND, ReviewEffective, ReviewScope, ReviewSource,
        SCHEMA, WIDGET_KEY,
    };
    use pi_rpc::{UiMethod, UiRequest};
    use serde_json::{Value, json};

    use super::{Report, Review};

    fn widget(record: Value) -> UiRequest {
        UiRequest {
            id: "ui-1".into(),
            timeout: None,
            method: UiMethod::SetWidget {
                key: WIDGET_KEY.into(),
                lines: Some(vec![record.to_string()]),
                placement: None,
            },
        }
    }

    fn capabilities(methods: &[&str], events: &[&str]) -> Capabilities {
        Capabilities {
            methods: methods.iter().map(|name| (*name).to_string()).collect(),
            events: events.iter().map(|name| (*name).to_string()).collect(),
            schema: Some(SCHEMA.into()),
            command: Some(PROMPT_COMMAND.into()),
            widget: Some(WIDGET_KEY.into()),
        }
    }

    fn ready() -> (Review, DesktopSession) {
        let mut review = Review::default();
        review.set_capabilities(capabilities(
            &[
                "capabilities",
                "get_snapshot",
                "get_review_mode",
                "set_review_mode",
            ],
            &["review_mode_changed"],
        ));
        (review, DesktopSession::new())
    }

    fn mode(effective: &str, source: &str, global: &str, clone: &str) -> Value {
        json!({"effective": effective, "source": source, "scopes": {"global": global, "clone": clone}})
    }

    fn ok(request_id: &str, data: Value) -> UiRequest {
        widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": request_id, "ok": true, "data": data,
        }))
    }

    fn error(request_id: &str, code: &str) -> UiRequest {
        widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": request_id, "ok": false,
            "error": {"code": code, "message": format!("{code} said the server")},
        }))
    }

    fn params(review: &mut Review, session: &mut DesktopSession) -> (String, Value) {
        let applied = review.pump(session);
        assert_eq!(applied.sends.len(), 1, "one request");
        let request = &applied.sends[0].request;
        let payload: Value = serde_json::from_str(request.payload()).unwrap();
        (request.request_id().to_string(), payload["params"].clone())
    }

    fn confirmation(enabled: bool, scope: &str, token: &str) -> Value {
        json!({
            "status": "confirmation_required",
            "enabled": enabled,
            "scope": scope,
            "mode": mode("enabled", "default", "unset", "unset"),
            "confirmation": {
                "title": "Turn review mode off for this repository?",
                "message": "Reviews will be off here.",
                "token": token,
            },
        })
    }

    /// Reads the switch once the probe lands, then the whole two-call round.
    fn seeded() -> (Review, DesktopSession) {
        let (mut review, mut session) = ready();
        let (id, params) = params(&mut review, &mut session);
        assert_eq!(params, json!({}));
        let _ = review.apply(
            &mut session,
            &ok(&id, mode("enabled", "default", "unset", "unset")),
        );
        (review, session)
    }

    #[test]
    fn a_shell_without_the_methods_never_shows_or_sends_anything() {
        let mut review = Review::default();
        review.set_capabilities(capabilities(&["capabilities", "get_snapshot"], &[]));
        let mut session = DesktopSession::new();
        assert!(review.pump(&mut session).sends.is_empty());
        assert!(review.mode().is_none());
        assert!(!review.request(false, ReviewScope::Clone));
        assert!(review.pump(&mut session).sends.is_empty());
        assert!(session.pending().is_none());
    }

    #[test]
    fn the_read_shows_the_switch_and_waits_for_a_busy_slot() {
        let (mut review, mut session) = ready();
        let _busy = session
            .issue(pi_rpc::desktop_rpc::DesktopMethod::GetSnapshot)
            .unwrap();
        assert!(review.pump(&mut session).sends.is_empty(), "waits");
        session.abandon();
        let (id, _) = params(&mut review, &mut session);
        let _ = review.apply(
            &mut session,
            &ok(&id, mode("disabled", "global", "disabled", "unset")),
        );
        let shown = review.mode().unwrap();
        assert_eq!(shown.effective, ReviewEffective::Disabled);
        assert_eq!(shown.source, ReviewSource::Global);
        assert!(review.pump(&mut session).sends.is_empty(), "read once");
    }

    #[test]
    fn no_review_owner_hides_the_control_silently() {
        let (mut review, mut session) = ready();
        let (id, _) = params(&mut review, &mut session);
        let applied = review.apply(&mut session, &error(&id, "review_unavailable"));
        assert!(applied.report.is_none());
        assert!(review.mode().is_none());
        review.refresh();
        assert!(
            review.pump(&mut session).sends.is_empty(),
            "not asked again"
        );
    }

    #[test]
    fn a_change_is_confirmed_by_the_human_then_resent_with_its_token() {
        let (mut review, mut session) = seeded();
        assert!(review.request(false, ReviewScope::Clone));
        assert!(review.changing());
        let (id, sent) = params(&mut review, &mut session);
        assert_eq!(
            sent,
            json!({"enabled": false, "scope": "clone"}),
            "no token"
        );
        let _ = review.apply(&mut session, &ok(&id, confirmation(false, "clone", "t1")));
        let shown = review.confirmation().unwrap();
        assert_eq!(shown.title, "Turn review mode off for this repository?");
        assert_eq!(shown.message, "Reviews will be off here.");
        assert!(!review.changing());
        assert!(
            review.pump(&mut session).sends.is_empty(),
            "waits for the human"
        );

        assert!(review.confirm());
        let (id, sent) = params(&mut review, &mut session);
        assert_eq!(
            sent,
            json!({"enabled": false, "scope": "clone", "confirmationToken": "t1"})
        );
        let applied = review.apply(
            &mut session,
            &ok(
                &id,
                json!({
                    "status": "applied", "enabled": false, "scope": "clone", "changed": true,
                    "mode": mode("disabled", "clone", "unset", "disabled"),
                }),
            ),
        );
        assert!(applied.report.is_none());
        assert!(review.confirmation().is_none());
        assert_eq!(review.mode().unwrap().source, ReviewSource::Clone);
    }

    #[test]
    fn a_stale_token_shows_the_fresh_confirmation_and_resends_only_that_one() {
        let (mut review, mut session) = seeded();
        review.request(false, ReviewScope::Global);
        let (id, _) = params(&mut review, &mut session);
        let _ = review.apply(&mut session, &ok(&id, confirmation(false, "global", "t1")));
        review.confirm();
        let (id, _) = params(&mut review, &mut session);
        // The switch moved in the TUI meanwhile: asked again, nothing written.
        let _ = review.apply(&mut session, &ok(&id, confirmation(false, "global", "t2")));
        assert!(review.confirmation().is_some());
        review.confirm();
        let (_, sent) = params(&mut review, &mut session);
        assert_eq!(sent["confirmationToken"], "t2");
    }

    #[test]
    fn declining_sends_nothing() {
        let (mut review, mut session) = seeded();
        review.request(true, ReviewScope::Clone);
        let (id, _) = params(&mut review, &mut session);
        let _ = review.apply(&mut session, &ok(&id, confirmation(true, "clone", "t1")));
        review.dismiss();
        assert!(review.confirmation().is_none());
        assert!(!review.confirm());
        assert!(review.pump(&mut session).sends.is_empty());
    }

    #[test]
    fn failures_are_reported_and_an_unknown_outcome_is_read_again() {
        let (mut review, mut session) = seeded();
        review.request(false, ReviewScope::Clone);
        let (id, _) = params(&mut review, &mut session);
        let applied = review.apply(&mut session, &error(&id, "review_mode_busy"));
        assert_eq!(applied.report, Some(Report::Busy));
        assert!(review.pump(&mut session).sends.is_empty(), "never retried");

        review.request(false, ReviewScope::Clone);
        let (id, _) = params(&mut review, &mut session);
        let applied = review.apply(&mut session, &error(&id, "review_unavailable"));
        assert_eq!(
            applied.report,
            Some(Report::Unavailable(
                "review_unavailable said the server".into()
            ))
        );
        let (_, read) = params(&mut review, &mut session);
        assert_eq!(read, json!({}), "re-read, not rewritten");
    }

    #[test]
    fn a_timeout_rolls_back_and_reads_again_and_a_stale_timer_does_nothing() {
        let (mut review, mut session) = seeded();
        review.request(false, ReviewScope::Clone);
        let applied = review.pump(&mut session);
        let generation = applied.sends[0].generation;
        assert!(review.expire(&mut session, generation + 1).report.is_none());
        assert_eq!(
            review.expire(&mut session, generation).report,
            Some(Report::Timeout)
        );
        assert!(session.pending().is_none());
        assert!(!review.changing());
        let (_, read) = params(&mut review, &mut session);
        assert_eq!(read, json!({}));
    }

    #[test]
    fn the_advertised_event_moves_the_switch() {
        let (mut review, mut session) = seeded();
        let event = |seq: u64, name: &str| {
            widget(json!({
                "schema": SCHEMA, "kind": "event", "seq": seq, "event": name,
                "data": {
                    "mode": mode("disabled", "global", "disabled", "unset"),
                    "previousMode": mode("enabled", "default", "unset", "unset"),
                },
            }))
        };
        let _ = review.apply(&mut session, &event(2, "review_mode_changed_v2"));
        assert_eq!(review.mode().unwrap().effective, ReviewEffective::Enabled);
        let _ = review.apply(&mut session, &event(3, "review_mode_changed"));
        assert_eq!(review.mode().unwrap().effective, ReviewEffective::Disabled);
        // A gap: reported for the channel's resync, and the switch re-read.
        let applied = review.apply(&mut session, &event(9, "review_mode_changed"));
        assert_eq!(applied.report, Some(Report::OutOfOrderSequence));
        let (_, read) = params(&mut review, &mut session);
        assert_eq!(read, json!({}));
    }

    #[test]
    fn reset_forgets_the_switch_and_a_late_answer_is_benign() {
        let (mut review, mut session) = seeded();
        review.request(false, ReviewScope::Clone);
        let (id, _) = params(&mut review, &mut session);
        review.reset();
        session.reset();
        assert!(review.mode().is_none());
        let applied = review.apply(&mut session, &ok(&id, confirmation(false, "clone", "t1")));
        assert!(applied.report.is_none());
        assert!(review.confirmation().is_none());
    }
}
