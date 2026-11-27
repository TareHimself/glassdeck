//! Callbacks that can wait. A host whose API waits (like `net.get`) runs each callback as a coroutine;
//! a coroutine that waits is parked under the ids it waits on, and resumed once every one is answered.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use anyhow::{Result, bail};
use mlua::thread::ThreadStatus;
use mlua::{Function, IntoLuaMulti, Lua, Thread, Value};

use crate::{LuaResultExt, Vm};

/// What a waiting call is waiting on, handed out by `Waits::next_id`.
pub type WaitId = u64;

/// `wait(ids)` in Luau: records what the running callback waits on, then yields to the scheduler.
const WAIT_GLUE: &str = r#"
local park = ...
return function(ids)
    park(ids)
    return coroutine.yield()
end
"#;

#[derive(Default)]
pub(crate) struct Waiting {
    next: Cell<WaitId>,
    /// The callback coroutine running right now, if any: only it may wait.
    running: RefCell<Option<Thread>>,
    /// What the running coroutine said it waits on, just before it yielded.
    asked: RefCell<Option<Vec<WaitId>>>,
    parked: RefCell<Vec<Parked>>,
    answers: RefCell<HashMap<WaitId, Value>>,
}

struct Parked {
    thread: Thread,
    on: Vec<WaitId>,
}

/// The host's handle on the scheduler, for an API that waits.
#[derive(Clone)]
pub struct Waits(Rc<Waiting>);

impl Waits {
    pub fn next_id(&self) -> WaitId {
        let id = self.0.next.get();
        self.0.next.set(id + 1);
        id
    }

    /// Fails unless the code running now is a callback the host started, the only place a wait can pause.
    pub fn check_can_wait(&self, lua: &Lua, what: &str) -> mlua::Result<()> {
        let current = lua.current_thread();
        match &*self.0.running.borrow() {
            Some(running) if running.to_pointer() == current.to_pointer() => Ok(()),
            _ => Err(mlua::Error::runtime(format!(
                "{what} waits, so it only works inside a callback the host runs (every, bridge.on, script.on_options), not at the top of the script or in a coroutine of its own"
            ))),
        }
    }

    /// The answer to `id`, for the coroutine waiting on it; `Vm::resume_answered` hands it over.
    pub fn answer(&self, id: WaitId, value: Value) {
        self.0.answers.borrow_mut().insert(id, value);
    }
}

impl Vm {
    /// Lets this VM's callbacks wait. Returns the scheduler's handle and `wait(ids)`, a Luau function an
    /// API calls after starting its work: it pauses the callback and returns the answers, in `ids` order.
    /// Call before `seal`.
    pub fn allow_waiting(&mut self) -> Result<(Waits, Function)> {
        let waiting = Rc::new(Waiting::default());
        let parked = Rc::clone(&waiting);
        let park = self
            .lua
            .create_function(move |lua, ids: Vec<WaitId>| {
                Waits(Rc::clone(&parked)).check_can_wait(lua, "waiting")?;
                *parked.asked.borrow_mut() = Some(ids);
                Ok(())
            })
            .lua_context("creating wait")?;
        let wait: Function = self.lua.load(WAIT_GLUE).set_name("wait").call(park).lua_context("creating wait")?;
        self.waiting = Some(Rc::clone(&waiting));
        Ok((Waits(waiting), wait))
    }

    /// Runs a callback whose result nobody needs: as a coroutine that may wait when waiting is allowed,
    /// else as a plain call.
    pub fn run(&self, function: &Function, args: impl IntoLuaMulti) -> Result<()> {
        let Some(waiting) = &self.waiting else { return self.call(function, args) };
        let thread = self.lua.create_thread(function.clone()).lua_context("starting a callback")?;
        self.resume(waiting, thread, args)
    }

    /// Resumes every parked callback whose answers are all in; drops answers nobody waits on any more.
    pub fn resume_answered(&self) -> Result<()> {
        let Some(waiting) = &self.waiting else { return Ok(()) };
        loop {
            let ready = {
                let answers = waiting.answers.borrow();
                let mut parked = waiting.parked.borrow_mut();
                parked.iter().position(|p| p.on.iter().all(|id| answers.contains_key(id))).map(|i| parked.remove(i))
            };
            let Some(Parked { thread, on }) = ready else { break };
            let answers = {
                let mut answers = waiting.answers.borrow_mut();
                on.iter().map(|id| answers.remove(id).expect("checked above")).collect::<Vec<_>>()
            };
            let table = self.lua.create_sequence_from(answers).lua_context("handing over answers")?;
            self.resume(waiting, thread, table)?;
        }
        let parked = waiting.parked.borrow();
        waiting.answers.borrow_mut().retain(|id, _| parked.iter().any(|p| p.on.contains(id)));
        Ok(())
    }

    /// Whether a callback is parked, waiting on an answer.
    pub fn is_waiting(&self) -> bool {
        self.waiting.as_ref().is_some_and(|w| !w.parked.borrow().is_empty())
    }

    fn resume(&self, waiting: &Waiting, thread: Thread, args: impl IntoLuaMulti) -> Result<()> {
        *waiting.running.borrow_mut() = Some(thread.clone());
        let resumed = self.guarded(|| thread.resume::<mlua::MultiValue>(args));
        *waiting.running.borrow_mut() = None;
        let asked = waiting.asked.borrow_mut().take();
        resumed?;
        match (thread.status(), asked) {
            (ThreadStatus::Resumable, Some(on)) => waiting.parked.borrow_mut().push(Parked { thread, on }),
            (ThreadStatus::Resumable, None) => bail!("a callback yielded without waiting on anything; only waits like net.get may pause it"),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `slow(n)` waits on a fresh id and returns its answer; the test answers by hand.
    fn waiting_vm() -> (Vm, Waits) {
        let mut vm = Vm::new().unwrap();
        let (waits, wait) = vm.allow_waiting().unwrap();
        let handle = waits.clone();
        let start = vm
            .lua()
            .create_function(move |lua, ()| {
                handle.check_can_wait(lua, "slow")?;
                Ok(handle.next_id())
            })
            .unwrap();
        let slow: Function = vm.lua().load("local start, wait = ... return function() return wait({ start() })[1] end").call((start, wait)).unwrap();
        vm.lua().globals().set("slow", slow).unwrap();
        vm.seal().unwrap();
        (vm, waits)
    }

    #[test]
    fn a_callback_waits_and_resumes_with_its_answer_while_others_run() {
        let (vm, waits) = waiting_vm();
        vm.load::<()>("got = {} function first() table.insert(got, 'first ' .. slow()) end function second() table.insert(got, 'second') end", "t").unwrap();
        let globals = vm.lua().globals();
        vm.run(&globals.get::<Function>("first").unwrap(), ()).unwrap();
        assert!(vm.is_waiting());
        vm.run(&globals.get::<Function>("second").unwrap(), ()).unwrap();
        waits.answer(0, Value::Integer(7));
        vm.resume_answered().unwrap();
        assert!(!vm.is_waiting());
        assert_eq!(vm.load::<String>("return table.concat(got, ', ')", "t").unwrap(), "second, first 7");
    }

    #[test]
    fn waiting_outside_a_callback_or_in_a_coroutine_of_its_own_is_an_error() {
        let (vm, _) = waiting_vm();
        let top = format!("{:#}", vm.load::<Value>("return slow()", "t").unwrap_err());
        assert!(top.contains("only works inside a callback"), "{top}");
        vm.load::<()>("function own() coroutine.wrap(function() slow() end)() end", "t").unwrap();
        let own = format!("{:#}", vm.run(&vm.lua().globals().get::<Function>("own").unwrap(), ()).unwrap_err());
        assert!(own.contains("only works inside a callback"), "{own}");
    }

    #[test]
    fn a_bare_yield_is_refused_and_late_answers_are_dropped() {
        let (vm, waits) = waiting_vm();
        vm.load::<()>("function bare() coroutine.yield() end", "t").unwrap();
        let bare = format!("{:#}", vm.run(&vm.lua().globals().get::<Function>("bare").unwrap(), ()).unwrap_err());
        assert!(bare.contains("yielded without waiting"), "{bare}");
        waits.answer(99, Value::Boolean(true));
        vm.resume_answered().unwrap();
        assert!(vm.waiting.as_ref().unwrap().answers.borrow().is_empty());
    }
}
