//! A background job's output tail (`get_job_output`) and a subagent's
//! activity thread (`get_subagent_thread`) over the channel, in one tab.
//!
//! Both are bounded reads with no event: the tab polls the job it shows
//! while it runs, and re-reads the thread on open and on each
//! `agents_update`. One job and one thread are watched at a time, the ones on
//! screen. Each read waits for the shared session's single slot, so it never
//! races the probe or another slice. A Shell that does not advertise a
//! method (4.8.0) is never asked: the views keep what they showed before.

use gentle_core::activity::{Thread, ThreadItem};
use pi_rpc::desktop_rpc::{
    Capabilities, DesktopErrorCode, DesktopJob, DesktopMethod, DesktopRecord, DesktopResponse,
    DesktopRpcError, DesktopSession, JobOutput, SubagentThread, ThreadItem as RpcThreadItem,
};

use super::Send;

/// The requests one decision produced. Failures are shown where the data
/// would be, never as conversation warnings: a poll must not spam them.
#[derive(Debug, Default)]
pub(in crate::features::session) struct Applied {
    pub(in crate::features::session) sends: Vec<Send>,
}

/// Why a read has nothing to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::features::session) enum Failure {
    /// `not_found`: the job or helper is gone (or was never this session's).
    Gone,
    /// `jobs_unavailable` / `agents_unavailable`: the owner cannot read it.
    Unavailable,
    /// A timeout, a transport or decoding failure.
    Failed,
}

/// Whether a job still runs, as the jobs list reads its status.
pub(in crate::features::session) fn is_running(job: &DesktopJob) -> bool {
    matches!(job.status.as_str(), "running" | "started" | "")
}

/// The job whose output is on screen.
pub(in crate::features::session) struct JobWatch {
    pub(in crate::features::session) id: String,
    /// The last tail read, plain text (the server strips control sequences).
    pub(in crate::features::session) output: Option<JobOutput>,
    pub(in crate::features::session) failure: Option<Failure>,
    /// A read is owed.
    due: bool,
    /// The job ended (or failed to read): no more polls.
    ended: bool,
}

/// The helper whose thread is on screen.
pub(in crate::features::session) struct ThreadWatch {
    pub(in crate::features::session) id: String,
    /// The bounded window, as the activity widget would show it.
    pub(in crate::features::session) thread: Option<Thread>,
    /// The store holds more than the window carries.
    pub(in crate::features::session) truncated: bool,
    pub(in crate::features::session) failure: Option<Failure>,
    due: bool,
    /// The version of the last read, sent back as `sinceVersion`.
    version: Option<u64>,
}

/// What the request in flight asked, to match its answer to the watch.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InFlight {
    Job(String),
    Thread { id: String, since: Option<u64> },
}

#[derive(Default)]
pub(in crate::features::session) struct Output {
    capabilities: Option<Capabilities>,
    job: Option<JobWatch>,
    thread: Option<ThreadWatch>,
    in_flight: Option<InFlight>,
    /// Bumped per request, so an old timer cannot drop a newer one.
    generation: u64,
}

impl Output {
    pub(in crate::features::session) fn set_capabilities(&mut self, capabilities: Capabilities) {
        self.capabilities = Some(capabilities);
    }

    fn supports(&self, method: DesktopMethod) -> bool {
        self.capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.supports(method))
    }

    /// Whether a job's output can be read on this backend.
    pub(in crate::features::session) fn job_supported(&self) -> bool {
        self.supports(DesktopMethod::GetJobOutput)
    }

    /// Whether a helper's thread can be read on this backend.
    pub(in crate::features::session) fn thread_supported(&self) -> bool {
        self.supports(DesktopMethod::GetSubagentThread)
    }

    pub(in crate::features::session) fn job(&self) -> Option<&JobWatch> {
        self.job.as_ref()
    }

    pub(in crate::features::session) fn thread(&self) -> Option<&ThreadWatch> {
        self.thread.as_ref()
    }

    /// Show `id`'s output: a first read is owed at once. Watching the job
    /// already watched changes nothing.
    pub(in crate::features::session) fn watch_job(&mut self, id: &str) {
        if !self.job_supported() || self.job.as_ref().is_some_and(|job| job.id == id) {
            return;
        }
        self.job = Some(JobWatch {
            id: id.to_string(),
            output: None,
            failure: None,
            due: true,
            ended: false,
        });
    }

    pub(in crate::features::session) fn unwatch_job(&mut self) {
        self.job = None;
    }

    /// One poll tick: another read is owed while the job runs. Whether it is
    /// still worth polling.
    pub(in crate::features::session) fn tick_job(&mut self) -> bool {
        match self.job.as_mut() {
            Some(job) if !job.ended => {
                job.due = true;
                true
            }
            _ => false,
        }
    }

    /// Show `id`'s thread: a first read is owed at once. The same id keeps
    /// what it has.
    pub(in crate::features::session) fn watch_thread(&mut self, id: &str) {
        if !self.thread_supported() || self.thread.as_ref().is_some_and(|t| t.id == id) {
            return;
        }
        self.thread = Some(ThreadWatch {
            id: id.to_string(),
            thread: None,
            truncated: false,
            failure: None,
            due: true,
            version: None,
        });
    }

    pub(in crate::features::session) fn unwatch_thread(&mut self) {
        self.thread = None;
    }

    /// The helpers moved (`agents_update`): re-read the watched thread. An
    /// unchanged thread answers no items, so this stays cheap.
    pub(in crate::features::session) fn refresh_thread(&mut self) {
        if let Some(thread) = self.thread.as_mut() {
            thread.due = true;
        }
    }

    /// Send the owed read, if the shared slot is free. The job's poll goes
    /// first: it is the one on a clock.
    pub(in crate::features::session) fn pump(&mut self, session: &mut DesktopSession) -> Applied {
        if session.pending().is_some() {
            return Applied::default();
        }
        if let Some(job) = self.job.as_mut().filter(|job| job.due) {
            job.due = false;
            match session.issue_get_job_output(&job.id) {
                Ok(request) => {
                    self.in_flight = Some(InFlight::Job(job.id.clone()));
                    return self.send(request);
                }
                Err(_) => {
                    job.failure = Some(Failure::Failed);
                    job.ended = true;
                }
            }
        }
        if let Some(thread) = self.thread.as_mut().filter(|thread| thread.due) {
            thread.due = false;
            match session.issue_get_subagent_thread(&thread.id, thread.version) {
                Ok(request) => {
                    self.in_flight = Some(InFlight::Thread {
                        id: thread.id.clone(),
                        since: thread.version,
                    });
                    return self.send(request);
                }
                Err(_) => thread.failure = Some(Failure::Failed),
            }
        }
        Applied::default()
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

    /// One incoming record the wiring routed here: the answer to this
    /// slice's request in flight.
    /// `decoded` is the request's [`pi_rpc::desktop_rpc::decode_widget`]
    /// result, so the record is not decoded again.
    pub(in crate::features::session) fn apply_decoded(
        &mut self,
        session: &mut DesktopSession,
        decoded: Result<Option<DesktopRecord>, DesktopRpcError>,
    ) -> Applied {
        let awaiting = session.pending().map(|(_, method)| method);
        if let Ok(Some(DesktopRecord::Response(response))) = session.accept_decoded(decoded) {
            let asked = self.in_flight.take();
            match (awaiting, asked) {
                (Some(DesktopMethod::GetJobOutput), Some(InFlight::Job(id))) => {
                    self.apply_job(&id, &response)
                }
                (Some(DesktopMethod::GetSubagentThread), Some(InFlight::Thread { id, since })) => {
                    self.apply_thread(&id, since, response)
                }
                _ => {}
            }
        }
        Applied::default()
    }

    /// The pending timeout for `generation` fired: drop the read. A job's
    /// next tick asks again; a thread with nothing shown says it failed.
    pub(in crate::features::session) fn expire(
        &mut self,
        session: &mut DesktopSession,
        generation: u64,
    ) -> Applied {
        if generation == self.generation && owns_pending(session) {
            self.rollback(session);
        }
        Applied::default()
    }

    /// Pi answered the outer prompt; anything but `handled` drops the read.
    pub(in crate::features::session) fn outer(
        &mut self,
        session: &mut DesktopSession,
        generation: u64,
        handled: bool,
    ) -> Applied {
        if !handled && generation == self.generation && owns_pending(session) {
            self.rollback(session);
        }
        Applied::default()
    }

    /// The backend was replaced or the conversation changed: ids from the
    /// old one mean nothing, so the views close their watches.
    pub(in crate::features::session) fn reset(&mut self) {
        self.capabilities = None;
        self.job = None;
        self.thread = None;
        self.in_flight = None;
        self.generation += 1;
    }

    fn send(&mut self, request: pi_rpc::desktop_rpc::DesktopRequest) -> Applied {
        self.generation += 1;
        Applied {
            sends: vec![Send {
                request,
                generation: self.generation,
            }],
        }
    }

    fn rollback(&mut self, session: &mut DesktopSession) {
        session.abandon();
        if let Some(InFlight::Thread { id, .. }) = self.in_flight.take()
            && let Some(thread) = self.thread.as_mut().filter(|thread| thread.id == id)
            && thread.thread.is_none()
        {
            thread.failure = Some(Failure::Failed);
        }
        self.generation += 1;
    }

    fn apply_job(&mut self, id: &str, response: &DesktopResponse) {
        // Another job is on screen now: its own read is already owed.
        let Some(job) = self.job.as_mut().filter(|job| job.id == id) else {
            return;
        };
        match response.job_output() {
            Ok(output) => {
                job.ended = !is_running(&output.job);
                job.failure = None;
                job.output = Some(output);
            }
            Err(error) => {
                job.failure = Some(failure(&error, DesktopErrorCode::JobsUnavailable));
                job.ended = true;
            }
        }
    }

    fn apply_thread(&mut self, id: &str, since: Option<u64>, response: DesktopResponse) {
        let Some(watch) = self.thread.as_mut().filter(|thread| thread.id == id) else {
            return;
        };
        match response.subagent_thread() {
            // Nothing changed since the read on screen.
            Ok(read) if since == Some(read.version) && read.items.is_empty() => {
                watch.failure = None;
            }
            Ok(read) => {
                watch.version = Some(read.version);
                watch.truncated = read.truncated;
                watch.failure = None;
                watch.thread = Some(thread_from_rpc(read));
            }
            Err(error) => {
                watch.failure = Some(failure(&error, DesktopErrorCode::AgentsUnavailable));
            }
        }
    }
}

/// Whether the pending request is one this slice issued.
fn owns_pending(session: &DesktopSession) -> bool {
    matches!(
        session.pending().map(|(_, method)| method),
        Some(DesktopMethod::GetJobOutput) | Some(DesktopMethod::GetSubagentThread)
    )
}

fn failure(error: &DesktopRpcError, unavailable: DesktopErrorCode) -> Failure {
    match error {
        DesktopRpcError::RemoteError(info) => match info.kind() {
            Some(DesktopErrorCode::NotFound) => Failure::Gone,
            Some(code) if code == unavailable => Failure::Unavailable,
            _ => Failure::Failed,
        },
        _ => Failure::Failed,
    }
}

/// A `get_subagent_thread` window as the activity widget's thread, so the
/// Activity tab draws both the same way.
pub(in crate::features::session) fn thread_from_rpc(read: SubagentThread) -> Thread {
    Thread {
        dropped: read.dropped,
        items: read
            .items
            .into_iter()
            .map(|item| match item {
                RpcThreadItem::Text { text } => ThreadItem::Text { text },
                RpcThreadItem::Thinking { text } => ThreadItem::Thinking { text },
                RpcThreadItem::Note { text } => ThreadItem::Note { text },
                RpcThreadItem::Tool {
                    name,
                    args,
                    running,
                    is_error,
                    output,
                } => ThreadItem::Tool {
                    name,
                    args,
                    running,
                    is_error,
                    output: (!output.is_empty()).then_some(output),
                },
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{
        Capabilities, DesktopMethod, DesktopSession, PROMPT_COMMAND, SCHEMA, WIDGET_KEY,
    };
    use pi_rpc::{UiMethod, UiRequest};
    use serde_json::{Value, json};

    use super::{Failure, Output};
    use gentle_core::activity::ThreadItem;

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

    fn ok(request_id: &str, data: Value) -> UiRequest {
        widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": request_id, "ok": true, "data": data,
        }))
    }

    fn error(request_id: &str, code: &str) -> UiRequest {
        widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": request_id, "ok": false,
            "error": {"code": code, "message": "no"},
        }))
    }

    fn capabilities(methods: &[&str]) -> Capabilities {
        let mut session = DesktopSession::new();
        let request = session.issue(DesktopMethod::Capabilities).unwrap();
        let mut all = vec!["capabilities", "get_snapshot"];
        all.extend_from_slice(methods);
        let answer = ok(
            request.request_id(),
            json!({
                "schema": SCHEMA, "command": PROMPT_COMMAND.trim_start_matches('/'),
                "widget": WIDGET_KEY, "methods": all, "events": [],
            }),
        );
        match session.accept(&answer) {
            Ok(Some(pi_rpc::desktop_rpc::DesktopRecord::Response(response))) => {
                response.capabilities().unwrap()
            }
            other => panic!("{other:?}"),
        }
    }

    fn params(send: &super::Send) -> Value {
        serde_json::from_str::<Value>(send.request.payload()).unwrap()["params"].clone()
    }

    fn ready(methods: &[&str]) -> (Output, DesktopSession) {
        let mut output = Output::default();
        output.set_capabilities(capabilities(methods));
        (output, DesktopSession::new())
    }

    fn job_output(status: &str, output: &str, truncated: bool) -> Value {
        json!({
            "job": {"id": "job-1", "label": "npm test", "status": status, "startedAt": 1},
            "output": output,
            "truncated": truncated,
        })
    }

    #[test]
    fn a_shell_without_the_methods_is_never_asked() {
        let (mut output, mut session) = ready(&[]);
        output.watch_job("job-1");
        output.watch_thread("agent-1");
        assert!(output.job().is_none() && output.thread().is_none());
        assert!(output.pump(&mut session).sends.is_empty());
        assert!(session.pending().is_none());
    }

    #[test]
    fn a_running_job_is_polled_until_it_ends() {
        let (mut output, mut session) = ready(&["get_job_output"]);
        output.watch_job("job-1");
        let sends = output.pump(&mut session).sends;
        assert_eq!(sends.len(), 1);
        assert_eq!(sends[0].request.method(), DesktopMethod::GetJobOutput);
        assert_eq!(params(&sends[0]), json!({"id": "job-1"}));
        // Nothing more until the answer and the next tick.
        assert!(output.pump(&mut session).sends.is_empty());

        let id = sends[0].request.request_id().to_string();
        output.apply(&mut session, &ok(&id, job_output("running", "a\nb", false)));
        let job = output.job().unwrap();
        assert_eq!(job.output.as_ref().unwrap().output, "a\nb");
        assert!(output.tick_job(), "a running job is polled again");
        let sends = output.pump(&mut session).sends;
        let id = sends[0].request.request_id().to_string();
        output.apply(
            &mut session,
            &ok(&id, job_output("succeeded", "a\nb\ndone", true)),
        );
        assert!(output.job().unwrap().output.as_ref().unwrap().truncated);
        assert!(!output.tick_job(), "an ended job is not polled");
        assert!(output.pump(&mut session).sends.is_empty());
    }

    #[test]
    fn a_read_waits_for_the_shared_slot() {
        let (mut output, mut session) = ready(&["get_job_output"]);
        let _probe = session.issue(DesktopMethod::GetSnapshot).unwrap();
        output.watch_job("job-1");
        assert!(output.pump(&mut session).sends.is_empty());
        session.abandon();
        assert_eq!(output.pump(&mut session).sends.len(), 1);
    }

    #[test]
    fn a_gone_job_stops_polling_and_says_so() {
        let (mut output, mut session) = ready(&["get_job_output"]);
        output.watch_job("job-1");
        let id = output.pump(&mut session).sends[0]
            .request
            .request_id()
            .to_string();
        output.apply(&mut session, &error(&id, "not_found"));
        assert_eq!(output.job().unwrap().failure, Some(Failure::Gone));
        assert!(!output.tick_job());
    }

    #[test]
    fn an_answer_for_a_job_no_longer_shown_is_dropped() {
        let (mut output, mut session) = ready(&["get_job_output"]);
        output.watch_job("job-1");
        let id = output.pump(&mut session).sends[0]
            .request
            .request_id()
            .to_string();
        output.watch_job("job-2");
        output.apply(&mut session, &ok(&id, job_output("running", "old", false)));
        assert!(output.job().unwrap().output.is_none());
        // job-2's own read is owed and goes out now that the slot is free.
        let sends = output.pump(&mut session).sends;
        assert_eq!(params(&sends[0]), json!({"id": "job-2"}));
    }

    #[test]
    fn a_timed_out_read_frees_the_slot_and_the_next_tick_asks_again() {
        let (mut output, mut session) = ready(&["get_job_output"]);
        output.watch_job("job-1");
        let generation = output.pump(&mut session).sends[0].generation;
        output.expire(&mut session, generation);
        assert!(session.pending().is_none());
        assert!(output.tick_job());
        assert_eq!(output.pump(&mut session).sends.len(), 1);
    }

    #[test]
    fn the_thread_is_read_then_reread_with_its_version() {
        let (mut output, mut session) = ready(&["get_subagent_thread"]);
        output.watch_thread("agent-1");
        let sends = output.pump(&mut session).sends;
        assert_eq!(params(&sends[0]), json!({"id": "agent-1"}));
        let id = sends[0].request.request_id().to_string();
        output.apply(
            &mut session,
            &ok(
                &id,
                json!({
                    "id": "agent-1", "version": 7, "dropped": 2, "truncated": true,
                    "items": [
                        {"kind": "text", "text": "Mapping"},
                        {"kind": "tool", "name": "read", "args": "{\"path\":\"a\"}",
                         "running": false, "isError": false, "output": ""},
                    ],
                }),
            ),
        );
        let watch = output.thread().unwrap();
        let thread = watch.thread.as_ref().unwrap();
        assert!(watch.truncated);
        assert_eq!(thread.dropped, 2);
        assert_eq!(
            thread.items[0],
            ThreadItem::Text {
                text: "Mapping".into()
            }
        );
        assert!(matches!(
            &thread.items[1],
            ThreadItem::Tool { output: None, .. }
        ));

        // agents_update: re-read with the version; unchanged keeps the view.
        output.refresh_thread();
        let sends = output.pump(&mut session).sends;
        assert_eq!(
            params(&sends[0]),
            json!({"id": "agent-1", "sinceVersion": 7})
        );
        let id = sends[0].request.request_id().to_string();
        output.apply(
            &mut session,
            &ok(
                &id,
                json!({"id": "agent-1", "version": 7, "dropped": 2, "truncated": false, "items": []}),
            ),
        );
        let watch = output.thread().unwrap();
        assert_eq!(watch.thread.as_ref().unwrap().items.len(), 2);
        assert!(
            watch.truncated,
            "an unchanged answer keeps the window it had"
        );
    }

    #[test]
    fn a_thread_failure_is_shown_where_the_thread_would_be() {
        let (mut output, mut session) = ready(&["get_subagent_thread"]);
        output.watch_thread("agent-1");
        let id = output.pump(&mut session).sends[0]
            .request
            .request_id()
            .to_string();
        output.apply(&mut session, &error(&id, "agents_unavailable"));
        assert_eq!(output.thread().unwrap().failure, Some(Failure::Unavailable));

        output.watch_thread("agent-2");
        let generation = output.pump(&mut session).sends[0].generation;
        output.outer(&mut session, generation, false);
        assert!(session.pending().is_none());
        assert_eq!(output.thread().unwrap().failure, Some(Failure::Failed));
    }

    #[test]
    fn reset_forgets_the_watches_and_a_late_answer_is_benign() {
        let (mut output, mut session) = ready(&["get_job_output"]);
        output.watch_job("job-1");
        let id = output.pump(&mut session).sends[0]
            .request
            .request_id()
            .to_string();
        output.reset();
        session.reset();
        output.apply(&mut session, &ok(&id, job_output("running", "x", false)));
        assert!(output.job().is_none());
        assert!(!output.job_supported());
    }
}
