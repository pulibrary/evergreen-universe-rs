use super::addr::{ClientAddress, ServiceAddress};
use super::app;
use super::client::{Client, ClientSingleton};
use super::conf;
use super::message;
use super::message::Message;
use super::message::MessageStatus;
use super::message::MessageType;
use super::message::Payload;
use super::message::TransportMessage;
use super::method;
use super::method::ParamCount;
use super::sclient::HostSettings;
use super::session::ServerSession;
use std::cell::RefMut;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time;

// How often each worker wakes to check for shutdown signals, etc.
const POLL_TIME: i32 = 5;

/// Each worker thread is in one of these states.
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum WorkerState {
    Idle,
    Active,
    Done,
}

#[derive(Debug)]
pub struct WorkerStateEvent {
    pub worker_id: u64,
    pub state: WorkerState,
}

impl WorkerStateEvent {
    pub fn worker_id(&self) -> u64 {
        self.worker_id
    }
    pub fn state(&self) -> WorkerState {
        self.state
    }
}

/// A Worker runs in its own thread and responds to API requests.
pub struct Worker {
    service: String,

    config: Arc<conf::Config>,

    stopping: Arc<AtomicBool>,

    // Settings from opensrf.settings
    host_settings: Arc<HostSettings>,

    client: Client,

    // True if the caller has requested a stateful conversation.
    connected: bool,

    methods: Arc<HashMap<String, method::Method>>,

    // Currently active session.
    // A worker can only have one active session at a time.
    // For stateless requests, each new thread results in a new session.
    // Starting a new thread/session in a stateful conversation
    // results in an error.
    session: Option<ServerSession>,

    worker_id: u64,

    // Channel for sending worker state info to our parent.
    to_parent_tx: mpsc::SyncSender<WorkerStateEvent>,
}

impl fmt::Display for Worker {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Worker ({})", self.worker_id)
    }
}

impl Worker {
    pub fn new(
        service: String,
        worker_id: u64,
        config: Arc<conf::Config>,
        host_settings: Arc<HostSettings>,
        stopping: Arc<AtomicBool>,
        methods: Arc<HashMap<String, method::Method>>,
        to_parent_tx: mpsc::SyncSender<WorkerStateEvent>,
    ) -> Result<Worker, String> {
        let client = Client::connect(config.clone())?;

        Ok(Worker {
            config,
            host_settings,
            stopping,
            service,
            worker_id,
            methods,
            client,
            to_parent_tx,
            session: None,
            connected: false,
        })
    }

    /// Mutable Ref to our under-the-covers client singleton.
    fn client_internal_mut(&self) -> RefMut<ClientSingleton> {
        self.client.singleton().borrow_mut()
    }

    /// Current session
    ///
    /// Panics of session on None.
    fn session(&self) -> &ServerSession {
        self.session.as_ref().unwrap()
    }

    fn session_mut(&mut self) -> &mut ServerSession {
        self.session.as_mut().unwrap()
    }

    pub fn worker_id(&self) -> u64 {
        self.worker_id
    }

    pub fn create_app_worker(
        &mut self,
        factory: app::ApplicationWorkerFactory,
        env: Box<dyn app::ApplicationEnv>,
    ) -> Result<Box<dyn app::ApplicationWorker>, String> {
        let mut app_worker = (factory)();
        app_worker.absorb_env(
            self.client.clone(),
            self.config.clone(),
            self.host_settings.clone(),
            self.methods.clone(),
            env,
        )?;
        Ok(app_worker)
    }

    pub fn listen(&mut self, mut appworker: Box<dyn app::ApplicationWorker>) {
        let selfstr = format!("{self}");

        if let Err(e) = appworker.worker_start() {
            log::error!("{selfstr} worker_start failed {e}.  Exiting");
            return;
        }

        let max_requests: u32 = self
            .host_settings
            .value(&format!("apps/{}/unix_config/max_requests", self.service))
            .as_u32()
            .unwrap_or(5000);

        let keepalive: i32 = self
            .host_settings
            .value(&format!("apps/{}/unix_config/keepalive", self.service))
            .as_i32()
            .unwrap_or(5);

        let mut requests: u32 = 0;
        let service_addr = ServiceAddress::new(&self.service).as_str().to_string();
        let local_addr = self.client.address().as_str().to_string();

        while requests < max_requests {
            let timeout: i32;
            let sent_to: &str;

            if self.connected {
                // We're in the middle of a stateful conversation.
                // Listen for messages sent specifically to our bus
                // address and only wait up to keeplive seconds for
                // subsequent messages.
                sent_to = &local_addr;
                timeout = keepalive;
            } else {
                // If we are not within a stateful conversation, clear
                // our bus data and message backlogs since any remaining
                // is no longer relevant.
                if let Err(e) = self.reset() {
                    log::error!("{selfstr} could not reset {e}.  Exiting");
                    break;
                }

                // Wait indefinitely for top-level service messages.
                sent_to = &service_addr;
                timeout = POLL_TIME;
            }

            log::trace!(
                "{selfstr} calling recv() timeout={} sent_to={}",
                timeout,
                sent_to
            );

            let recv_op = self
                .client_internal_mut()
                .bus_mut()
                .recv(timeout, Some(sent_to));

            // True if this wake cycle performed any tasks.
            let mut work_occurred = false;

            match recv_op {
                Err(e) => {
                    log::error!("{selfstr} recv() in listen produced an error: {e}");

                    // There's a good chance an error in recv() means
                    // the thread/system is unusable, so let the worker
                    // exit.
                    //
                    // Avoid a tight thread respawn loop with a short pause.
                    thread::sleep(time::Duration::from_secs(1));
                    self.connected = false;
                    break;
                }

                Ok(recv_op) => {
                    match recv_op {
                        None => {
                            // See if the caller failed to send a follow-up
                            // request within the keepalive timeout.
                            if self.connected {
                                if let Err(e) = self.notify_state(WorkerState::Active) {
                                    log::error!("{self} failed to notify parent of Active state. Exiting. {e}");
                                    break;
                                }

                                log::warn!("{selfstr} timeout waiting on request while connected");
                                self.connected = false;
                                work_occurred = true;

                                if let Err(e) =
                                    self.reply_with_status(MessageStatus::Timeout, "Timeout")
                                {
                                    log::error!(
                                        "server: could not reply with Timeout message: {e}"
                                    );
                                }
                            }
                        }

                        Some(msg) => {
                            if let Err(e) = self.notify_state(WorkerState::Active) {
                                log::error!(
                                    "{self} failed to notify parent of Active state. Exiting. {e}"
                                );
                                break;
                            }

                            if let Err(e) = self.handle_transport_message(&msg, &mut appworker) {
                                log::error!("{selfstr} error handling message: {e}");
                                self.connected = false;
                            }

                            work_occurred = true;
                        }
                    }
                }
            }

            // If we are connected, we remain Active and avoid counting
            // subsequent requests within this stateful converstation
            // toward our overall request count.
            if self.connected {
                continue;
            }

            if work_occurred {
                // If we processed a message let the worker know a stateless
                // request or stateful conversation has just completed.
                if let Err(e) = appworker.end_session() {
                    log::error!("end_session() returned an error: {e}");
                    break;
                }

                if let Err(e) = self.notify_state(WorkerState::Idle) {
                    // If we can't notify our parent, it means the parent
                    // thread is no longer running.  Get outta here.
                    log::error!("{selfstr} could not notify parent of Idle state. Exiting. {e}");
                    break;
                }
            } else {
                // Nothing interesting happened.
                if let Err(e) = appworker.worker_idle_wake() {
                    log::error!("worker_idle_wake() returned an error: {e}");
                    break;
                }
            }

            // Did we get a shutdown signal?
            if self.stopping.load(Ordering::Relaxed) {
                log::info!("{selfstr} received a stop signal");
                break;
            }

            requests += 1;
        }

        log::trace!("{self} exiting listen loop");

        if let Err(e) = appworker.worker_end() {
            log::error!("{selfstr} worker_end failed {e}");
        }

        if let Err(_) = self.notify_state(WorkerState::Done) {
            log::error!("{self} failed to notify parent of Done state");
        }

        // Clear our worker-specific bus address of any lingering data.
        self.reset().ok();
    }

    fn handle_transport_message(
        &mut self,
        tmsg: &message::TransportMessage,
        appworker: &mut Box<dyn app::ApplicationWorker>,
    ) -> Result<(), String> {
        if self.session.is_none() || self.session().thread().ne(tmsg.thread()) {
            log::trace!("server: creating new server session for {}", tmsg.thread());

            self.session = Some(ServerSession::new(
                self.client.clone(),
                &self.service,
                tmsg.thread(),
                0, // thread trace -- updated later as needed
                ClientAddress::from_string(tmsg.from())?,
            ));
        }

        for msg in tmsg.body().iter() {
            self.handle_message(msg, appworker)?;
        }

        Ok(())
    }

    // Clear our local message bus and reset state maintenance values.
    fn reset(&mut self) -> Result<(), String> {
        self.connected = false;
        self.session = None;
        self.client.clear()
    }

    fn handle_message(
        &mut self,
        msg: &message::Message,
        appworker: &mut Box<dyn app::ApplicationWorker>,
    ) -> Result<(), String> {
        self.session_mut().set_last_thread_trace(msg.thread_trace());
        self.session_mut().clear_responded_complete();

        log::trace!("{self} received message of type {:?}", msg.mtype());

        match msg.mtype() {
            message::MessageType::Disconnect => {
                log::trace!("{self} received a DISCONNECT");
                self.reset()?;
                Ok(())
            }

            message::MessageType::Connect => {
                log::trace!("{self} received a CONNECT");

                if self.connected {
                    return self.reply_bad_request("Worker is already connected");
                }

                self.connected = true;
                self.reply_with_status(MessageStatus::Ok, "OK")
            }

            message::MessageType::Request => {
                log::trace!("{self} received a REQUEST");
                self.handle_request(msg, appworker)
            }

            _ => self.reply_bad_request("Unexpected message type"),
        }
    }

    fn reply_with_status(&mut self, stat: MessageStatus, stat_text: &str) -> Result<(), String> {
        let tmsg = TransportMessage::with_body(
            self.session().sender().as_str(),
            self.client.address().as_str(),
            self.session().thread(),
            Message::new(
                MessageType::Status,
                self.session().last_thread_trace(),
                Payload::Status(message::Status::new(stat, stat_text, "osrfStatus")),
            ),
        );

        self.client_internal_mut()
            .get_domain_bus(self.session().sender().domain())?
            .send(&tmsg)
    }

    fn handle_request(
        &mut self,
        msg: &message::Message,
        appworker: &mut Box<dyn app::ApplicationWorker>,
    ) -> Result<(), String> {
        let request = match msg.payload() {
            message::Payload::Method(m) => m,
            _ => return self.reply_bad_request("Request sent without payload"),
        };

        let mut log_params: Option<String> = None;

        if self
            .config
            .log_protect()
            .iter()
            .filter(|m| request.method().starts_with(&m[..]))
            .next()
            .is_none()
        {
            log_params = Some(
                request
                    .params()
                    .iter()
                    .map(|p| p.dump())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        };

        let log_params = log_params.as_deref().unwrap_or("**PARAMS REDACTED**");

        // Log the API call
        log::info!("CALL: {} {}", request.method(), log_params);

        // Before we begin processing a service-level request, clear our
        // local message bus to avoid encountering any stale messages
        // lingering from the previous conversation.
        if !self.connected {
            self.client.clear()?;
        }

        let method = match self.methods.get(request.method()) {
            Some(m) => {
                // Clone the method since we have mutable borrows below.
                // Note this is the method definition, not the request,
                // which may have non-clone-friendly bulky parameters.
                m.clone()
            }
            None => {
                return self.reply_with_status(
                    MessageStatus::MethodNotFound,
                    &format!("Method not found: {}", request.method()),
                );
            }
        };

        if method.atomic() {
            self.session_mut().new_atomic_resp_queue();
        }

        let pcount = method.param_count();

        // Make sure the number of params sent by the caller matches the
        // parameter count for the method.
        if !ParamCount::matches(&pcount, request.params().len() as u8) {
            return self.reply_bad_request(&format!(
                "Invalid param count sent: method={} sent={} needed={}",
                request.method(),
                request.params().len(),
                &pcount,
            ));
        }

        // De-serialize the inbound parameters.
        let mut unpacked_params = Vec::new();
        if let Some(s) = self.client.singleton().borrow().serializer() {
            for p in request.params() {
                // TODO if these are unpacked at message create time,
                // we could avoid the clone.  Not a huge deal since
                // inbound params are typically small, but still.
                //
                // TODO we could verify at this point whether each
                // paramater matches its documented ParamDataType.
                unpacked_params.push(s.unpack(p.clone()));
            }
        }
        let request = message::Method::new(request.method(), unpacked_params);

        if let Err(ref err) = (method.handler())(appworker, self.session_mut(), &request) {
            let msg = format!("{self} method {} failed with {err}", request.method());
            log::error!("{msg}");
            appworker.api_call_error(&request, err);
            self.reply_server_error(&msg)?;
            Err(msg)?;
        }

        if !self.session().responded_complete() {
            self.session_mut().send_complete()
        } else {
            Ok(())
        }
    }

    fn reply_server_error(&mut self, text: &str) -> Result<(), String> {
        self.connected = false;

        let msg = Message::new(
            MessageType::Status,
            self.session().last_thread_trace(),
            Payload::Status(message::Status::new(
                MessageStatus::InternalServerError,
                &format!("Internal Server Error: {text}"),
                "osrfStatus",
            )),
        );

        let tmsg = TransportMessage::with_body(
            self.session().sender().as_str(),
            self.client.address().as_str(),
            self.session().thread(),
            msg,
        );

        self.client_internal_mut()
            .get_domain_bus(self.session().sender().domain())?
            .send(&tmsg)
    }

    fn reply_bad_request(&mut self, text: &str) -> Result<(), String> {
        self.connected = false;

        let msg = Message::new(
            MessageType::Status,
            self.session().last_thread_trace(),
            Payload::Status(message::Status::new(
                MessageStatus::BadRequest,
                &format!("Bad Request: {text}"),
                "osrfStatus",
            )),
        );

        let tmsg = TransportMessage::with_body(
            self.session().sender().as_str(),
            self.client.address().as_str(),
            self.session().thread(),
            msg,
        );

        self.client_internal_mut()
            .get_domain_bus(self.session().sender().domain())?
            .send(&tmsg)
    }

    /// Notify the parent process of this worker's active state.
    fn notify_state(&self, state: WorkerState) -> Result<(), mpsc::SendError<WorkerStateEvent>> {
        log::trace!("{self} notifying parent of state change => {state:?}");

        self.to_parent_tx.send(WorkerStateEvent {
            worker_id: self.worker_id(),
            state: state,
        })
    }
}
