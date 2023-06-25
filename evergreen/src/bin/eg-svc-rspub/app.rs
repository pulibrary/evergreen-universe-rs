use eg::idl;
use evergreen as eg;
use opensrf::app::{Application, ApplicationEnv, ApplicationWorker, ApplicationWorkerFactory};
use opensrf::client::Client;
use opensrf::conf;
use opensrf::message;
use opensrf::method::Method;
use opensrf::sclient::HostSettings;
use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

// Import our local methods module.
use crate::methods;

const APPNAME: &str = "open-ils.rspub";

/// Environment shared by all service workers.
///
/// The environment is only mutable up until the point our
/// Server starts spawning threads.
#[derive(Debug, Clone)]
pub struct RsPubEnv {
    /// Global / shared IDL ref
    idl: Arc<idl::Parser>,
}

impl RsPubEnv {
    pub fn new(idl: &Arc<idl::Parser>) -> Self {
        RsPubEnv { idl: idl.clone() }
    }

    pub fn idl(&self) -> &Arc<idl::Parser> {
        &self.idl
    }
}

/// Implement the needed Env trait
impl ApplicationEnv for RsPubEnv {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Our main application class.
pub struct RsPubApplication {
    /// We load the IDL during service init.
    idl: Option<Arc<idl::Parser>>,
}

impl RsPubApplication {
    pub fn new() -> Self {
        RsPubApplication { idl: None }
    }
}

impl Application for RsPubApplication {
    fn name(&self) -> &str {
        APPNAME
    }

    fn env(&self) -> Box<dyn ApplicationEnv> {
        Box::new(RsPubEnv::new(self.idl.as_ref().unwrap()))
    }

    /// Load the IDL and perform any other needed global startup work.
    fn init(
        &mut self,
        _client: Client,
        _config: Arc<conf::Config>,
        host_settings: Arc<HostSettings>,
    ) -> Result<(), String> {
        let idl_file = host_settings
            .value("IDL")
            .as_str()
            .ok_or(format!("No IDL path!"))?;

        let idl = idl::Parser::parse_file(&idl_file)
            .or_else(|e| Err(format!("Cannot parse IDL file: {e}")))?;

        self.idl = Some(idl);

        Ok(())
    }

    /// Tell the Server what methods we want to publish.
    fn register_methods(
        &self,
        _client: Client,
        _config: Arc<conf::Config>,
        _host_settings: Arc<HostSettings>,
    ) -> Result<Vec<Method>, String> {
        let mut methods: Vec<Method> = Vec::new();

        // Create Method objects from our static method definitions.
        for def in methods::METHODS.iter() {
            methods.push(def.into_method(APPNAME));
        }

        // NOTE here is where additional, dynamically created methods
        // could be appended.

        Ok(methods)
    }

    fn worker_factory(&self) -> ApplicationWorkerFactory {
        || Box::new(RsPubWorker::new())
    }
}

/// Per-thread worker instance.
pub struct RsPubWorker {
    env: Option<RsPubEnv>,
    client: Option<Client>,
    config: Option<Arc<conf::Config>>,
    host_settings: Option<Arc<HostSettings>>,
    methods: Option<Arc<HashMap<String, Method>>>,
}

impl RsPubWorker {
    pub fn new() -> Self {
        RsPubWorker {
            env: None,
            client: None,
            config: None,
            methods: None,
            host_settings: None,
        }
    }

    /// This will only ever be called after absorb_env(), so we are
    /// guarenteed to have an env.
    pub fn env(&self) -> &RsPubEnv {
        self.env.as_ref().unwrap()
    }

    /// Cast a generic ApplicationWorker into our RsPubWorker.
    ///
    /// This is necessary to access methods/fields on our RsPubWorker that
    /// are not part of the ApplicationWorker trait.
    pub fn downcast(w: &mut Box<dyn ApplicationWorker>) -> Result<&mut RsPubWorker, String> {
        match w.as_any_mut().downcast_mut::<RsPubWorker>() {
            Some(eref) => Ok(eref),
            None => Err(format!("Cannot downcast")),
        }
    }

    /// Ref to our OpenSRF client.
    ///
    /// Set during absorb_env()
    pub fn client(&self) -> &Client {
        self.client.as_ref().unwrap()
    }

    /// Mutable ref to our OpenSRF client.
    ///
    /// Set during absorb_env()
    pub fn client_mut(&mut self) -> &mut Client {
        self.client.as_mut().unwrap()
    }
}

impl ApplicationWorker for RsPubWorker {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn methods(&self) -> &Arc<HashMap<String, Method>> {
        &self.methods.as_ref().unwrap()
    }

    /// Absorb our global dataset.
    ///
    /// Panics if we cannot downcast the env provided to the expected type.
    fn absorb_env(
        &mut self,
        client: Client,
        config: Arc<conf::Config>,
        host_settings: Arc<HostSettings>,
        methods: Arc<HashMap<String, Method>>,
        env: Box<dyn ApplicationEnv>,
    ) -> Result<(), String> {
        let worker_env = env
            .as_any()
            .downcast_ref::<RsPubEnv>()
            .ok_or(format!("Unexpected environment type in absorb_env()"))?;

        // Each worker gets its own client, so we have to tell our
        // client how to pack/unpack network data.
        client.set_serializer(idl::Parser::as_serializer(worker_env.idl()));

        self.env = Some(worker_env.clone());
        self.client = Some(client);
        self.config = Some(config);
        self.methods = Some(methods);
        self.host_settings = Some(host_settings);

        Ok(())
    }

    /// Called before the worker goes into its listen state.
    fn worker_start(&mut self) -> Result<(), String> {
        log::debug!("Thread starting");
        Ok(())
    }

    fn worker_idle_wake(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// Called after all requests are handled and the worker is
    /// shutting down.
    fn worker_end(&mut self) -> Result<(), String> {
        log::debug!("Thread ending");
        Ok(())
    }

    fn end_session(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn keepalive_timeout(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn api_call_error(&mut self, _request: &message::Method, _error: &str) {}
}
