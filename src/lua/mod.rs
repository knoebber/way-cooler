//! Lua functionality

use hlua;
use hlua::{Lua, LuaError};
use hlua::any::AnyLuaValue;

use rustc_serialize::json::Json;

use std::thread;
use std::fs::{File};
use std::path::Path;
use std::io::Write;
use std::fmt::{Debug, Formatter};
use std::fmt::Result as FmtResult;

use std::sync::{Mutex, RwLock};
use std::sync::mpsc::{channel, Sender, Receiver};

#[macro_use]
mod funcs;
#[cfg(test)]
mod tests;

lazy_static! {
    /// Sends requests to the lua thread
    static ref SENDER: Mutex<Option<Sender<LuaMessage>>> = Mutex::new(None);

    /// Receives data back from the lua thread
    /// This should only be accessed by the lua thread itself.
    static ref RECEIVER: Mutex<Option<Receiver<LuaResponse>>> = Mutex::new(None);

    /// Whether the lua thread is currently running
    pub static ref RUNNING: RwLock<bool> = RwLock::new(false);
}

/// Represents an identifier for dealing with nested tables.
///
/// To access foo.bar.baz, use vec!["foo", "bar", "baz"].
///
/// To access foo[2], use vec!["foo", 2].
pub type LuaIdentifier = Vec<AnyLuaValue>;

/// Messages sent to the lua thread
#[derive(Debug)]
pub enum LuaQuery {
    /// Pings the lua thread
    Ping,
    /// Halt the lua thread
    Terminate,
    // Restart the lua thread
    Restart,

    /// Execute a string
    Execute(String),
    /// Execute a file
    ExecFile(String),

    /// Get a variable, expecting an AnyLuaValue
    GetValue(LuaIdentifier),
    /// Invoke a function found at the position,
    /// with the specified arguments.
    Invoke(LuaIdentifier, Vec<AnyLuaValue>),
    /// Set a value
    SetValue {
        /// The name of the thing to stuff
        name: LuaIdentifier,
        /// The value to store.
        val: Json
    },
    /// Create a new table
    NewTable(LuaIdentifier),
}

/// Messages received from lua thread
pub enum LuaResponse {
    /// Lua variable obtained
    Variable(Option<AnyLuaValue>),
    /// Lua error
    Error(hlua::LuaError),
    /// A function is returned
    Function(hlua::functions_read::LuaFunction<String>),
    /// Pong response from lua ping
    Pong,
}

#[derive(Debug)]
struct LuaMessage {
    reply: Sender<LuaResponse>,
    query: LuaQuery
}

unsafe impl Send for LuaQuery { }
unsafe impl Sync for LuaQuery { }
unsafe impl Send for LuaResponse { }
unsafe impl Sync for LuaResponse { }
unsafe impl Send for LuaMessage { }
unsafe impl Sync for LuaMessage { }

impl Debug for LuaResponse {
    fn fmt(&self, f: &mut Formatter) -> FmtResult {
        match *self {
            LuaResponse::Variable(ref var) =>
                write!(f, "LuaResponse::Variable({:?})", var),
            LuaResponse::Error(ref err) =>
                write!(f, "LuaResponse::Error({:?})", err),
            LuaResponse::Function(_) =>
                write!(f, "LuaResponse::Function"),
            LuaResponse::Pong =>
                write!(f, "LuaResponse::Pong")
        }
    }
}


/// Whether the lua thread is currently available
pub fn thread_running() -> bool {
    *RUNNING.read().unwrap()
}

/// Errors which may arise from attempting
/// to sending a message to the lua thread.
#[derive(Debug)]
pub enum LuaSendError {
    /// The thread crashed, was shut down, or rebooted.
    ThreadClosed,
    /// The thread has not been initialized yet (maybe not used)
    ThreadUninitialized,
    /// The sender had an issue, most likey because the thread panicked.
    /// Following the `Sender` API, the original value sent is returned.
    Sender(LuaQuery)
}

/// Attemps to send a LuaQuery to the lua thread.
pub fn try_send(query: LuaQuery) -> Result<Receiver<LuaResponse>,LuaSendError> {
    if !thread_running() {
        Err(LuaSendError::ThreadClosed)
    }
    else if let Some(ref sender) = *SENDER.lock().unwrap() {
        let (tx, rx) = channel();
        let message = LuaMessage { reply: tx, query: query };
        match sender.send(message) {
            Ok(_) => Ok(rx),
            Err(e) => Err(LuaSendError::Sender(e.0.query))
        }
    }
    else {
        Err(LuaSendError::ThreadUninitialized)
    }
}

/// Initialize the lua thread
pub fn init() {
    trace!("Initializing...");
    let (query_tx, query_rx) = channel::<LuaMessage>();
    let (answer_tx, answer_rx) = channel::<LuaResponse>();
    {
        let mut sender = SENDER.lock().unwrap();
        let mut receiver = RECEIVER.lock().unwrap();

        *sender = Some(query_tx);
        *receiver = Some(answer_rx);
    }

    thread::spawn(move || {
        thread_init(answer_tx, query_rx);
    });
    trace!("Created thread. Init finished.");
}

fn thread_init(sender: Sender<LuaResponse>, receiver: Receiver<LuaQuery>) {
    trace!("thread: initializing.");
    let mut lua = Lua::new();
    //unsafe {
    //    hlua_ffi::lua_atpanic(&mut lua.as_mut_lua().0, thread_on_panic);
    //}
    debug!("thread: Loading Lua libraries...");
    lua.openlibs();
    trace!("thread: Loading way-cooler lua extensions...");
    // We should have some good file handling, read files from /usr by default,
    // but for now we're reading directly from the source.
    lua.execute_from_reader::<(), File>(
        File::open("lib/lua/init.lua").unwrap()
    ).unwrap();
    trace!("thread: loading way-cooler libraries...");
    funcs::register_libraries(&mut lua);
    // Only ready after loading libs
    *RUNNING.write().unwrap() = true;
    debug!("thread: entering main loop...");
    thread_main_loop(sender, receiver, &mut lua);
}

fn thread_main_loop(sender: Sender<LuaResponse>, receiver: Receiver<LuaQuery>,
                    lua: &mut Lua) {
    loop {
        let request = receiver.recv();
        match request {
            Err(e) => {
                error!("Lua thread: unable to receive message: {}", e);
                error!("Lua thread: now panicking!");
                *RUNNING.write().unwrap() = false;

                panic!("Lua thread: lost contact with host, exiting!");
            }
            Ok(message) => {
                trace!("Handling a request");
                thread_handle_message(&sender, message, lua);
            }
        }
    }
}

fn thread_handle_message(sender: &Sender<LuaResponse>,
                         request: LuaQuery, lua: &mut Lua) {
    match request {
        LuaQuery::Terminate => {
            trace!("thread: Received terminate signal");
            *RUNNING.write().unwrap() = false;

            info!("thread: Lua thread terminating!");
            return;
        },

        LuaQuery::Restart => {
            trace!("thread: Received restart signal!");
            error!("thread: Lua thread restart not supported!");

            *RUNNING.write().unwrap() = false;

            panic!("Lua thread: Restart not supported!");
        },

        LuaQuery::Execute(code) => {
            trace!("thread: Received request to execute code");
            trace!("thread: Executing {:?}", code);

            match lua.execute::<()>(&code) {
                Err(error) => {
                    warn!("thread: Error executing code: {:?}", error);
                    let response = LuaResponse::Error(error);

                    thread_send(&sender, response);
                }
                Ok(_) => {
                    // This is gonna be really spammy one day
                    trace!("thread: Code executed okay.");
                }
            }
        },

        LuaQuery::ExecFile(name) => {
            trace!("thread: Received request to execute file {}", name);
            info!("thread: Executing {}", name);

            let path = Path::new(&name);
            let try_file = File::open(path);

            if let Ok(file) = try_file {
                let result = lua.execute_from_reader::<(), File>(file);
                if let Err(err) = result {
                    warn!("thread: Error executing {}!", name);

                    thread_send(&sender, LuaResponse::Error(err));
                }
                else {
                    trace!("thread: Execution of {} successful.", name);
                }
            }
            else { // Could not open file
                // Unwrap_err is used because we're in the else of let
                let read_error =
                    LuaError::ReadError(try_file.unwrap_err());

                thread_send(&sender, LuaResponse::Error(read_error));
            }
        },

        LuaQuery::GetValue(varname) => {
            trace!("thread: Received request to get variable {:?}", varname);
            let var_result = lua.get(format!("{:?}", varname));

            match var_result {
                Some(var) => {
                    thread_send(&sender, LuaResponse::Variable(Some(var)));
                }
                None => {
                    warn!("thread: Unable to get variable {:?}", varname);

                    thread_send(&sender, LuaResponse::Variable(None));
                }
            }
        },

        LuaQuery::SetValue { name: _name, val: _val } => {
            panic!("thread: unimplemented LuaQuery::SetValue!");
        },

        LuaQuery::NewTable(_name) => {
            panic!("thread: unimplemented LuaQuery::NewTable!");
        },

        LuaQuery::Ping => {
            panic!("thread: unimplemented LuaQuery::Ping!");
        },
        _ => {
            panic!("Unimplemented send type for lua thread!");
        }
    }
}

fn thread_send(sender: &Sender<LuaResponse>, response: LuaResponse) {
    trace!("Called thread_send");
    match sender.send(response) {
        Err(_) => {
            error!("thread: Unable to broadcast response!");
            error!("thread: Shutting down in response to inability \
                    to continue!");
            panic!("Lua thread unable to communicate with main thread, \
                    shutting down!");
        }
        Ok(_) => {}
    }
}
