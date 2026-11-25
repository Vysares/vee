//! The command router in the loop: a hand written executive with two command sinks, two `Link`
//! modules that submit commands into them (the SAME module type, as in `queued_bus_loop.rs`), a
//! `Worker` module that is a command target, and the real `Core`, which is the router and also a
//! target itself. It shows what `exec!` will have to generate for commands, and checks the whole
//! path: a sink, `Core::route_commands`, the ack, the target's queue, and the target's own step.
//!
//! The order inside a tick is the one `design.md` gives: each module's suppressed report, then
//! `core.route_commands`, then `core.tick` (which runs the core's own commands), then the modules.
//! So a command submitted in tick N is routed at the start of tick N+1.
//!
//! What it checks:
//! - Where a command goes: to a module's queue or the core's own, and what happens to one for
//!   nobody (rejected, with the reason in the ack).
//! - Every way `accept` can refuse: unknown opcode, args of the wrong size, a value `validate`
//!   refuses, a full target queue. A rejected command is never run.
//! - The sinks: a module only pushes; ids come from the sink, not the module; a full sink tells
//!   its submitter; one sink is drained fully, oldest first, before the next; a drained sink takes
//!   commands again.
//! - Acks: sent by the core, never throttled, with the sink, seq, target and opcode in the text.
//!
//! Not here: the outcome ack of a MODULE's command. `cmd::drain` needs the whole `Ev`, which a
//! module's `Ctx` does not have, so `Worker` pops its queue itself and reports with its own event.
//! (The core's own commands do go through `drain`: see the `SetEventFilter` tests.)

#![allow(
    clippy::unwrap_used,
    reason = "a test file: a failed unwrap is a failed test (the helpers are not `#[test]` fns, \
              so clippy.toml's exemption does not cover them)"
)]

use vee_core::slot::Slot;
use vee_core_api::cmd;
use vee_core_api::ctx;
use vee_core_api::event;
use vee_core_api::module;
use vee_core_api::queue;
use vee_core_api::throttle;
use vee_osal::clock::Clock as _;
use vee_osal::fake_clock;
use vee_osal::fake_files;
use vee_osal::fake_platform;
use vee_osal::fake_watchdog;

const EVENT_LOG: &str = "events.log";
const RESET_LOG: &str = "reset.log";

/// The time between the start of one tick and the start of the next. Tick `n` starts at
/// `(n + 1) * TICK_US`.
const TICK_US: u64 = 10_000;
/// How much the fake clock moves after each module's call, so each call sees its own time.
const MODULE_COST_US: u64 = 100;

/// The modules' ids. `WORKER` is also its command target id.
const LINK_GROUND: u8 = 1;
const LINK_RTS: u8 = 2;
const WORKER: u8 = 3;
/// The sinks' ids. They are deliberately not the modules' ids: a sink is a source of commands
/// (ground, an RTS), and a module is only what feeds it.
const SINK_GROUND: u8 = 11;
const SINK_RTS: u8 = 12;

/// A target nothing is registered for.
const NOBODY: u8 = 9;

/// The core's one command, `SetEventFilter`.
const SET_EVENT_FILTER: u16 = 1;

/// How many commands the worker's queue holds. Small, so a test can fill it.
const WORKER_QUEUE_DEPTH: usize = 2;
/// `Set` with a value above this fails.
const SET_MAX: u16 = 1000;

// ---- The worker's commands ----

#[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
#[validate]
struct AddArgs {
    amount: u16,
}

impl AddArgs {
    fn validate(&self) -> Result<(), cmd::CmdErr> {
        if self.amount > 0 {
            Ok(())
        } else {
            Err(cmd::CmdErr::Invalid("amount must be nonzero"))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, cmd::Command)]
struct SetArgs {
    value: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, cmd::CmdSet)]
#[repr(u16)]
enum WorkerCmd {
    Add(AddArgs) = 1,
    Set(SetArgs) = 2,
}

const ADD: u16 = 1;
const SET: u16 = 2;

// ---- Module 1: the target ----

/// The worker's command queue and how a command gets into it: what a module's `CommandTarget`
/// will be. The executive owns it (the router needs it while the worker is not running), and
/// lends it to the router as a target and to the worker, through `In`, as its queue.
struct WorkerTarget {
    queue: queue::Queue<(WorkerCmd, cmd::CmdMeta), WORKER_QUEUE_DEPTH>,
}

impl cmd::CommandTarget for WorkerTarget {
    fn accept(&mut self, raw: &cmd::RawCmd) -> Result<(), cmd::CmdErr> {
        let command = WorkerCmd::decode(raw)?;
        command.validate()?;
        self.queue
            .push((command, cmd::CmdMeta::from(raw)))
            .map_err(|_| cmd::CmdErr::QueueFull)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum WorkerEv {
    #[severity(Info)]
    Handled,
}

struct Worker {
    total: u32,
}

impl Worker {
    /// What the module does for each command, and the result it reports. The module author
    /// writes only this: `drain_cmds` pops the commands and sends each outcome.
    fn handle(&mut self, command: WorkerCmd, ctx: &mut ctx::Ctx<'_, Self>) -> cmd::CmdResult {
        match command {
            WorkerCmd::Add(args) => {
                self.total = self.total.saturating_add(u32::from(args.amount));
                ctx.ev
                    .send(WorkerEv::Handled, format_args!("total {}", self.total));
                cmd::CmdResult::Success
            }
            WorkerCmd::Set(args) if args.value > SET_MAX => cmd::CmdResult::Fail("value too big"),
            WorkerCmd::Set(args) => {
                self.total = u32::from(args.value);
                ctx.ev
                    .send(WorkerEv::Handled, format_args!("total {}", self.total));
                // A zero is "started, not finished": the outcome is `pending`.
                if args.value == 0 {
                    cmd::CmdResult::Pending
                } else {
                    cmd::CmdResult::Success
                }
            }
        }
    }
}

impl module::Module for Worker {
    type Event = WorkerEv;
    type In<'a> = ();
    type Out<'a> = ();
    type Cmd = WorkerCmd;

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        ctx.drain_cmds(|command, ctx| self.handle(command, ctx));
    }
}

// ---- Module 2: a link. LINK_GROUND and LINK_RTS are both this SAME type: they differ only in
// which sink the executive points their handle at, and in their script. ----

/// One command a link submits.
#[derive(Clone)]
struct Submit {
    seq: u16,
    target: u8,
    opcode: u16,
    args: Vec<u8>,
}

fn add(seq: u16, amount: u16) -> Submit {
    Submit {
        seq,
        target: WORKER,
        opcode: ADD,
        args: AddArgs { amount }.encode().to_vec(),
    }
}

fn set(seq: u16, value: u16) -> Submit {
    Submit {
        seq,
        target: WORKER,
        opcode: SET,
        args: SetArgs { value }.encode().to_vec(),
    }
}

fn set_event_filter(seq: u16, floor: u8) -> Submit {
    Submit {
        seq,
        target: cmd::CORE_TARGET,
        opcode: SET_EVENT_FILTER,
        args: vec![floor],
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum LinkEv {
    #[severity(Info)]
    Submitted,
}

struct LinkOut<'a> {
    cmd: cmd::SinkHandle<'a>,
}

/// Submits `script[n]` at its `n`th step, and reports how many went in and how many the sink
/// refused (one event, so the event throttle never hides a count).
struct Link {
    script: Vec<Vec<Submit>>,
    steps: usize,
}

impl module::Module for Link {
    type Event = LinkEv;
    type In<'a> = ();
    type Out<'a> = LinkOut<'a>;
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        let batch = self.script.get(self.steps).cloned().unwrap_or_default();
        self.steps = self.steps.saturating_add(1);
        let mut accepted = 0_u32;
        let mut refused = 0_u32;
        for submit in batch {
            let args = cmd::Args::new(&submit.args).unwrap();
            match ctx
                .out
                .cmd
                .submit(submit.seq, submit.target, submit.opcode, args)
            {
                Ok(()) => accepted = accepted.saturating_add(1),
                Err(_) => refused = refused.saturating_add(1),
            }
        }
        ctx.ev.send(
            LinkEv::Submitted,
            format_args!("submitted {accepted}, refused {refused}"),
        );
    }
}

// ---- The executive ----

/// What `exec!` will generate: the sinks, the targets, a slot for each module, and the calls in
/// order.
struct Exec {
    clock: fake_clock::FakeClock,
    core: vee_core::Core<fake_platform::FakePlatform>,
    tick: u64,
    /// Index 0 is ground's, index 1 is the RTS's. The core drains them in this order.
    sinks: [cmd::Sink; 2],
    worker_target: WorkerTarget,
    ground: Slot<Link>,
    rts: Slot<Link>,
    worker: Slot<Worker>,
}

impl Exec {
    fn new(ground_script: Vec<Vec<Submit>>, rts_script: Vec<Vec<Submit>>) -> Self {
        let clock = fake_clock::FakeClock::new();
        let core = vee_core::Core::<fake_platform::FakePlatform>::start(
            fake_files::FakeFiles::new(),
            fake_watchdog::FakeWatchdog::new(),
            RESET_LOG,
            EVENT_LOG,
            clock.monotonic_us(),
        );
        Exec {
            clock,
            core,
            tick: 0,
            sinks: [cmd::Sink::new(SINK_GROUND), cmd::Sink::new(SINK_RTS)],
            worker_target: WorkerTarget {
                queue: queue::Queue::new(),
            },
            ground: Slot::new(
                Link {
                    script: ground_script,
                    steps: 0,
                },
                LINK_GROUND,
                1,
            ),
            rts: Slot::new(
                Link {
                    script: rts_script,
                    steps: 0,
                },
                LINK_RTS,
                1,
            ),
            worker: Slot::new(Worker { total: 0 }, WORKER, 1),
        }
    }

    /// One tick, in the order `design.md` gives.
    fn run_tick(&mut self) {
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.clock.set(now);

        // 1. Each module's report of what it suppressed (this also starts its new tick of
        //    throttle counting).
        self.ground.report_suppressed(&mut self.core.ev, now);
        self.rts.report_suppressed(&mut self.core.ev, now);
        self.worker.report_suppressed(&mut self.core.ev, now);

        // 2. The router: everything the sinks hold goes to its target's queue, and is acked.
        let mut targets: [(u8, &mut dyn cmd::CommandTarget); 1] =
            [(WORKER, &mut self.worker_target)];
        self.core.route_commands(&mut self.sinks, &mut targets, now);

        // 3. The core's own step, which runs its own queued commands.
        self.core.tick(now);

        // 4. The modules.
        let reset = self.core.reset();
        let time = self.clock.monotonic_us();
        let out = LinkOut {
            cmd: self.sinks[0].handle(),
        };
        self.ground.step(
            self.tick,
            &mut self.core.ev,
            time,
            reset,
            (),
            out,
            cmd::Inbox::empty(),
        );
        self.clock.advance(MODULE_COST_US);

        let time = self.clock.monotonic_us();
        let out = LinkOut {
            cmd: self.sinks[1].handle(),
        };
        self.rts.step(
            self.tick,
            &mut self.core.ev,
            time,
            reset,
            (),
            out,
            cmd::Inbox::empty(),
        );
        self.clock.advance(MODULE_COST_US);

        let time = self.clock.monotonic_us();
        let cmds = cmd::Inbox::new(&mut self.worker_target.queue);
        self.worker
            .step(self.tick, &mut self.core.ev, time, reset, (), (), cmds);
        self.clock.advance(MODULE_COST_US);

        self.tick = self.tick.saturating_add(1);
    }

    /// What one source reported, with its time, oldest first, after one more core tick has
    /// written the last tick's events to the event log.
    fn events(&mut self, source: u8) -> Vec<(u64, String)> {
        let now = self.tick.saturating_add(1).saturating_mul(TICK_US);
        self.core.tick(now);
        self.core.flush_event_log(now);
        let bytes = self.core.files().get(EVENT_LOG).unwrap_or_default();
        bytes
            .chunks(vee_core_types::event_log::RECORD_LEN)
            .map(|record| vee_core_types::event_log::decode(record).unwrap())
            .filter(|event| event.source == source)
            .map(|event| {
                (
                    event.monotonic_us,
                    std::str::from_utf8(event.text.as_bytes())
                        .unwrap()
                        .to_owned(),
                )
            })
            .collect()
    }

    /// Just the texts of `events`.
    fn texts(&mut self, source: u8) -> Vec<String> {
        self.events(source)
            .into_iter()
            .map(|(_, text)| text)
            .collect()
    }

    /// What the worker reported about its own work (not the outcome acks).
    fn totals(&mut self) -> Vec<String> {
        self.texts(WORKER)
            .into_iter()
            .filter(|text| !text.starts_with("cmd "))
            .collect()
    }

    /// The outcome acks the worker's `drain_cmds` sent, oldest first.
    fn outcomes(&mut self) -> Vec<String> {
        self.texts(WORKER)
            .into_iter()
            .filter(|text| text.starts_with("cmd "))
            .collect()
    }

    /// Every ack and outcome the core sent for a command, oldest first. (The core's other events,
    /// such as its reset report, are not commands.)
    fn acks(&mut self) -> Vec<String> {
        self.texts(cmd::CORE_TARGET)
            .into_iter()
            .filter(|text| text.starts_with("cmd "))
            .collect()
    }
}

/// An executive with only ground submitting, that has run `ticks` ticks.
fn ground_only(script: Vec<Vec<Submit>>, ticks: u64) -> Exec {
    run(script, Vec::new(), ticks)
}

fn run(ground: Vec<Vec<Submit>>, rts: Vec<Vec<Submit>>, ticks: u64) -> Exec {
    let mut exec = Exec::new(ground, rts);
    for _ in 0..ticks {
        exec.run_tick();
    }
    exec
}

// ---- Routing to a module ----

#[test]
fn a_command_waits_in_its_sink_until_the_next_tick() {
    // Submitted during tick 0's module phase: nothing has routed it yet.
    let mut exec = ground_only(vec![vec![add(1, 5)]], 1);
    assert_eq!(exec.texts(LINK_GROUND), ["submitted 1, refused 0"]);
    assert_eq!(exec.acks(), Vec::<String>::new());
    assert!(exec.totals().is_empty());
}

#[test]
fn a_command_is_routed_at_the_top_of_the_next_tick_and_handled_in_the_targets_step() {
    let mut exec = ground_only(vec![vec![add(1, 5)]], 2);

    // Routed and acked at the start of tick 1, whose time is 2 * TICK_US.
    let acks = exec.events(cmd::CORE_TARGET);
    let ack = acks
        .iter()
        .find(|(_, text)| text.starts_with("cmd "))
        .unwrap();
    assert_eq!(ack.1, "cmd 11/1 t3 op0x0001: accepted");
    assert_eq!(ack.0, 2 * TICK_US);

    // Handled in the worker's own step of the same tick, after the links' steps.
    let handled = exec.events(WORKER);
    assert_eq!(handled.len(), 2);
    assert_eq!(handled[0].1, "total 5");
    assert_eq!(handled[1].1, "cmd 11/1 t3 op0x0001: succeeded");
    for (time, _) in handled {
        assert!(time > 2 * TICK_US && time < 3 * TICK_US);
    }
}

#[test]
fn a_command_is_not_run_when_it_is_accepted() {
    // The worker is slower than the tick: it steps every 4th tick, so the command waits in its
    // queue. The ack for "received" is sent at once, the handling comes later.
    let mut exec = Exec::new(vec![vec![add(1, 5)]], Vec::new());
    exec.worker = Slot::new(Worker { total: 0 }, WORKER, 4);
    // Ticks 0 to 3: the worker stepped at tick 0 only, before the command was routed.
    for _ in 0..4 {
        exec.run_tick();
    }
    assert_eq!(exec.acks(), ["cmd 11/1 t3 op0x0001: accepted"]);
    assert!(exec.totals().is_empty());

    // Tick 4 is its next step (steps at ticks 0, 4, 8, ...).
    exec.run_tick();
    assert_eq!(exec.totals(), ["total 5"]);
}

#[test]
fn commands_are_handled_in_the_order_they_were_submitted() {
    let mut exec = ground_only(vec![vec![set(1, 10), add(2, 5)]], 2);
    assert_eq!(exec.totals(), ["total 10", "total 15"]);
}

#[test]
fn a_command_that_arrives_every_tick_is_handled_every_tick() {
    // Submitted at ticks 0 to 49, each handled one tick later, so after 50 ticks the last one is
    // still waiting in its sink.
    const TICKS: u16 = 50;
    let script = (0..TICKS).map(|n| vec![add(n, 1)]).collect();
    let mut exec = ground_only(script, u64::from(TICKS));

    let handled = exec.totals();
    assert_eq!(handled.len(), usize::from(TICKS) - 1);
    let last = TICKS - 1;
    assert_eq!(handled.last().unwrap(), &format!("total {last}"));
    assert_eq!(exec.outcomes().len(), usize::from(TICKS) - 1);
    assert_eq!(exec.acks().len(), usize::from(TICKS) - 1);
}

// ---- Every way a command is turned away ----

#[test]
fn a_command_for_a_target_nobody_has_is_rejected() {
    let command = Submit {
        target: NOBODY,
        ..add(1, 5)
    };
    let mut exec = ground_only(vec![vec![command]], 2);
    assert_eq!(
        exec.acks(),
        ["cmd 11/1 t9 op0x0001: rejected: unknown command target"]
    );
    assert!(exec.totals().is_empty());
}

#[test]
fn a_command_with_an_unknown_opcode_is_rejected_and_not_run() {
    let command = Submit {
        opcode: 99,
        ..add(1, 5)
    };
    let mut exec = ground_only(vec![vec![command]], 2);
    assert_eq!(
        exec.acks(),
        ["cmd 11/1 t3 op0x0063: rejected: unknown opcode"]
    );
    assert!(exec.totals().is_empty());
}

#[test]
fn a_command_with_args_of_the_wrong_size_is_rejected_and_not_run() {
    let command = Submit {
        args: vec![1],
        ..add(1, 5)
    };
    let mut exec = ground_only(vec![vec![command]], 2);
    assert_eq!(
        exec.acks(),
        ["cmd 11/1 t3 op0x0001: rejected: bad args size"]
    );
    assert!(exec.totals().is_empty());
}

#[test]
fn a_command_with_a_value_validate_refuses_is_rejected_with_the_reason() {
    let mut exec = ground_only(vec![vec![add(1, 0)]], 2);
    assert_eq!(
        exec.acks(),
        ["cmd 11/1 t3 op0x0001: rejected: invalid: amount must be nonzero"]
    );
    assert!(exec.totals().is_empty());
}

#[test]
fn a_rejected_command_does_not_stop_the_ones_after_it() {
    let mut exec = ground_only(vec![vec![add(1, 0), add(2, 7)]], 2);
    assert_eq!(
        exec.acks(),
        [
            "cmd 11/1 t3 op0x0001: rejected: invalid: amount must be nonzero",
            "cmd 11/2 t3 op0x0001: accepted",
        ]
    );
    assert_eq!(exec.totals(), ["total 7"]);
}

#[test]
fn a_full_target_queue_rejects_the_extra_command_and_keeps_the_rest() {
    // The worker's queue holds 2: the third in one tick is refused, the first two run.
    let mut exec = ground_only(vec![vec![add(1, 1), add(2, 2), add(3, 4)]], 2);
    assert_eq!(
        exec.acks(),
        [
            "cmd 11/1 t3 op0x0001: accepted",
            "cmd 11/2 t3 op0x0001: accepted",
            "cmd 11/3 t3 op0x0001: rejected: command queue is full",
        ]
    );
    assert_eq!(exec.totals(), ["total 1", "total 3"]);
}

#[test]
fn a_target_queue_that_was_emptied_takes_commands_again() {
    let mut exec = ground_only(
        vec![vec![add(1, 1), add(2, 1)], vec![add(3, 1), add(4, 1)]],
        3,
    );
    // Tick 1 routes and the worker empties its queue; tick 2 routes two more.
    assert_eq!(exec.acks().len(), 4);
    assert_eq!(exec.totals().last().unwrap(), "total 4");
}

// ---- The core as a target ----

#[test]
fn a_core_command_is_acked_when_routed_and_run_in_the_same_tick() {
    let mut exec = ground_only(vec![vec![set_event_filter(1, 1)]], 2);
    // Routed at the start of tick 1, and the core's own step runs it right after, in the same
    // tick, before any module.
    assert_eq!(
        exec.acks(),
        [
            "cmd 11/1 t0 op0x0001: accepted",
            "cmd 11/1 t0 op0x0001: succeeded",
        ]
    );
}

#[test]
fn the_core_command_changes_what_reaches_the_bus_queue() {
    // Floor 1 is Info: from then on every event also goes to the bus queue.
    let mut exec = ground_only(vec![vec![set_event_filter(1, 1)]], 1);
    assert!(exec.core.ev.bus.is_empty(), "the filter is off at first");
    exec.run_tick();
    assert!(!exec.core.ev.bus.is_empty());
}

#[test]
fn a_core_command_with_an_illegal_floor_is_rejected_and_changes_nothing() {
    let mut exec = ground_only(vec![vec![set_event_filter(1, 9)]], 3);
    assert_eq!(
        exec.acks(),
        ["cmd 11/1 t0 op0x0001: rejected: invalid: floor must be 0..=3"]
    );
    assert!(exec.core.ev.bus.is_empty());
}

#[test]
fn a_core_command_with_no_args_is_rejected() {
    let command = Submit {
        args: vec![],
        ..set_event_filter(1, 1)
    };
    let mut exec = ground_only(vec![vec![command]], 2);
    assert_eq!(
        exec.acks(),
        ["cmd 11/1 t0 op0x0001: rejected: bad args size"]
    );
}

#[test]
fn a_core_command_with_an_unknown_opcode_is_rejected() {
    let command = Submit {
        opcode: 77,
        ..set_event_filter(1, 1)
    };
    let mut exec = ground_only(vec![vec![command]], 2);
    assert_eq!(
        exec.acks(),
        ["cmd 11/1 t0 op0x004d: rejected: unknown opcode"]
    );
}

// ---- The sinks ----

#[test]
fn the_sinks_id_is_on_the_command_not_the_modules() {
    // The ground link is source 1, the sink is 11: the ack says 11. A module has no way to set
    // it (`SinkHandle` has no such argument), so this is the only id an ack can carry.
    let mut exec = ground_only(vec![vec![add(1, 1)]], 2);
    assert_eq!(exec.acks(), ["cmd 11/1 t3 op0x0001: accepted"]);
}

#[test]
fn two_sinks_are_drained_in_order_and_each_keeps_its_own_commands_in_order() {
    // Ground's two go first (sink 0, oldest first) and fill the worker's queue of 2, so the RTS's
    // (sink 1) finds it full. Had the RTS been drained first, its command would be the one
    // that ran.
    let mut exec = run(vec![vec![set(1, 10), add(2, 1)]], vec![vec![add(1, 5)]], 2);
    assert_eq!(
        exec.acks(),
        [
            "cmd 11/1 t3 op0x0002: accepted",
            "cmd 11/2 t3 op0x0001: accepted",
            "cmd 12/1 t3 op0x0001: rejected: command queue is full",
        ]
    );
    assert_eq!(exec.totals(), ["total 10", "total 11"]);
}

#[test]
fn a_command_from_each_sink_is_acked_with_its_own_sink_id() {
    let mut exec = run(vec![vec![set(1, 10)]], vec![vec![add(1, 5)]], 2);
    assert_eq!(
        exec.acks(),
        [
            "cmd 11/1 t3 op0x0002: accepted",
            "cmd 12/1 t3 op0x0001: accepted",
        ]
    );
    assert_eq!(exec.totals(), ["total 10", "total 15"]);
}

#[test]
fn a_full_sink_tells_the_module_and_keeps_what_it_has() {
    // One more than the sink holds, in one step. All go to nobody, so every ack is a rejection and
    // can be counted, whatever the other queues' depths.
    let depth = cmd::CMD_SINK_QUEUE_DEPTH;
    let batch = (0..=depth)
        .map(|n| Submit {
            seq: u16::try_from(n).unwrap(),
            target: NOBODY,
            ..add(0, 1)
        })
        .collect();
    let mut exec = ground_only(vec![batch], 2);

    assert_eq!(
        exec.texts(LINK_GROUND)[0],
        format!("submitted {depth}, refused 1")
    );
    let acks = exec.acks();
    assert_eq!(acks.len(), depth);
    // The refused one, the last submitted, never reached the router.
    assert!(
        acks.last()
            .unwrap()
            .starts_with(&format!("cmd 11/{} ", depth - 1))
    );
}

#[test]
fn a_sink_that_was_drained_takes_a_full_batch_again() {
    let depth = cmd::CMD_SINK_QUEUE_DEPTH;
    let batch = |first: usize| -> Vec<Submit> {
        (first..first + depth)
            .map(|n| Submit {
                seq: u16::try_from(n).unwrap(),
                target: NOBODY,
                ..add(0, 1)
            })
            .collect()
    };
    let mut exec = ground_only(vec![batch(0), batch(depth)], 3);
    assert_eq!(
        exec.texts(LINK_GROUND),
        [
            format!("submitted {depth}, refused 0"),
            format!("submitted {depth}, refused 0"),
            "submitted 0, refused 0".to_owned(),
        ]
    );
    assert_eq!(exec.acks().len(), 2 * depth);
}

// ---- Acks ----

#[test]
fn acks_are_not_throttled() {
    // The throttle lets one event code through `EV_MAX_PER_TICK` times a tick. This many acks of
    // one code in one tick are all there, because the core's own events skip it.
    let depth = cmd::CMD_SINK_QUEUE_DEPTH;
    assert!(
        depth > usize::from(throttle::EV_MAX_PER_TICK),
        "this test needs a sink that holds more commands than the throttle lets through"
    );
    let batch = (0..depth)
        .map(|n| Submit {
            seq: u16::try_from(n).unwrap(),
            target: NOBODY,
            ..add(0, 1)
        })
        .collect();
    let mut exec = ground_only(vec![batch], 2);
    assert_eq!(exec.acks().len(), depth);
}

#[test]
fn the_router_acks_come_from_the_core_and_the_outcomes_from_the_module_that_ran_the_command() {
    let mut exec = ground_only(vec![vec![add(1, 1), add(2, 0)]], 2);
    // Received / rejected: the router's, so the core's source.
    assert_eq!(
        exec.acks(),
        [
            "cmd 11/1 t3 op0x0001: accepted",
            "cmd 11/2 t3 op0x0001: rejected: invalid: amount must be nonzero",
        ]
    );
    // Succeeded: the worker ran it, so the worker's source. The rejected one never ran.
    assert_eq!(exec.outcomes(), ["cmd 11/1 t3 op0x0001: succeeded"]);
    // The link sent neither.
    assert!(
        exec.texts(LINK_GROUND)
            .iter()
            .all(|text| !text.starts_with("cmd "))
    );
}

#[test]
fn no_ack_is_cut_off_in_the_event_text() {
    let mut exec = ground_only(vec![vec![add(1, 1), add(2, 0), set(3, 5000)]], 2);
    for source in [cmd::CORE_TARGET, WORKER] {
        for (_, text) in exec.events(source) {
            assert!(!text.ends_with('~'), "cut off: {text}");
        }
    }
}

// ---- A module's outcome acks (`ctx.drain_cmds`) ----

#[test]
fn the_module_never_writes_an_outcome_it_is_sent_for_every_command_it_ran() {
    let mut exec = ground_only(vec![vec![add(1, 1), add(2, 2)]], 2);
    assert_eq!(
        exec.outcomes(),
        [
            "cmd 11/1 t3 op0x0001: succeeded",
            "cmd 11/2 t3 op0x0001: succeeded",
        ]
    );
}

#[test]
fn a_failed_command_gets_a_failed_outcome_with_the_reason_and_changes_nothing() {
    let mut exec = ground_only(vec![vec![set(1, 5000)]], 2);
    assert_eq!(
        exec.outcomes(),
        ["cmd 11/1 t3 op0x0002: failed: value too big"]
    );
    assert!(exec.totals().is_empty());
}

#[test]
fn a_pending_command_gets_a_pending_outcome() {
    let mut exec = ground_only(vec![vec![set(1, 0)]], 2);
    assert_eq!(exec.outcomes(), ["cmd 11/1 t3 op0x0002: pending"]);
}

#[test]
fn outcomes_come_in_the_order_the_commands_ran_with_each_ones_own_result() {
    let mut exec = ground_only(vec![vec![set(1, 7), set(2, 5000)]], 2);
    assert_eq!(
        exec.outcomes(),
        [
            "cmd 11/1 t3 op0x0002: succeeded",
            "cmd 11/2 t3 op0x0002: failed: value too big",
        ]
    );
}

#[test]
fn the_module_can_send_its_own_events_from_inside_the_handler() {
    // `total N` is sent by the module's handler, through the `ctx` it was given, before
    // `drain_cmds` sends the outcome.
    let mut exec = ground_only(vec![vec![add(1, 5)]], 2);
    let events = exec.texts(WORKER);
    assert_eq!(events, ["total 5", "cmd 11/1 t3 op0x0001: succeeded"]);
}

#[test]
fn the_outcome_is_sent_when_the_module_runs_the_command_not_when_it_is_accepted() {
    let mut exec = Exec::new(vec![vec![add(1, 5)]], Vec::new());
    exec.worker = Slot::new(Worker { total: 0 }, WORKER, 4);
    for _ in 0..4 {
        exec.run_tick();
    }
    // Accepted by the router long ago, but the worker has not stepped since.
    assert_eq!(exec.acks(), ["cmd 11/1 t3 op0x0001: accepted"]);
    assert!(exec.outcomes().is_empty());
    exec.run_tick();
    assert_eq!(exec.outcomes(), ["cmd 11/1 t3 op0x0001: succeeded"]);
}

#[test]
fn a_command_the_router_refused_gets_no_outcome() {
    let mut exec = ground_only(vec![vec![add(1, 0), set(2, 7)]], 2);
    assert_eq!(exec.outcomes(), ["cmd 11/2 t3 op0x0002: succeeded"]);
}

#[test]
fn the_outcome_has_the_workers_source_and_the_time_of_its_step() {
    let mut exec = ground_only(vec![vec![add(1, 5)]], 2);
    let events = exec.events(WORKER);
    let (time, text) = events.last().unwrap();
    assert_eq!(text, "cmd 11/1 t3 op0x0001: succeeded");
    // Tick 1 starts at 2 * TICK_US; the worker is the third module to step, after the two links.
    assert_eq!(*time, 2 * TICK_US + 2 * MODULE_COST_US);
}

#[test]
fn nothing_is_acked_when_nothing_is_submitted() {
    let mut exec = ground_only(vec![], 5);
    assert_eq!(exec.acks(), Vec::<String>::new());
}
