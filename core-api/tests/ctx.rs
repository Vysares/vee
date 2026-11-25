//! Tests of `Ctx` and `Module`, playing the part of the executive: build a `Ctx` for each call,
//! call the module, and look at what ended up in the event queue. (They are here, not in
//! `ctx.rs`, so they can use `#[derive(event::Code)]`.)

use vee_core_api::cmd;
use vee_core_api::ctx;
use vee_core_api::ev;
use vee_core_api::event;
use vee_core_api::module;
use vee_core_api::queue;
use vee_core_api::reset;
use vee_core_api::throttle;

const LIMIT: u8 = throttle::EV_MAX_PER_TICK;

// ---- A module that counts its steps ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum CounterEv {
    #[severity(Info)]
    Started,
    #[severity(Warn)]
    Stepped,
}

struct Counter {
    steps: u32,
}

impl module::Module for Counter {
    type Event = CounterEv;
    type In<'a> = ();
    type Out<'a> = ();
    type Cmd = ();

    fn init(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        let boot = ctx.reset.count;
        ctx.ev
            .send(CounterEv::Started, format_args!("started, boot {boot}"));
    }

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        self.steps = self.steps.saturating_add(1);
        let (steps, now) = (self.steps, ctx.monotonic());
        ctx.ev
            .send(CounterEv::Stepped, format_args!("step {steps} at {now}"));
    }
}

// ---- A module that sends the same event over and over ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum NoisyEv {
    #[severity(Err)]
    Spam,
    #[severity(Info)]
    Quiet,
}

struct Noisy;

impl module::Module for Noisy {
    type Event = NoisyEv;
    type In<'a> = ();
    type Out<'a> = ();
    type Cmd = ();

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        for _ in 0..u16::from(LIMIT) + 5 {
            ctx.ev.send(NoisyEv::Spam, format_args!("spam"));
        }
        ctx.ev.send(NoisyEv::Quiet, format_args!("quiet"));
    }
}

// ---- The executive's side ----

fn reset_info(count: u32) -> reset::ResetInfo {
    reset::ResetInfo {
        count,
        previous: reset::PreviousEnd::Unknown,
        consecutive_unclean: 2,
    }
}

/// What the executive does for one call of `init`: make a `Ctx`, and call the module.
fn run_init<'a, M: module::Module>(
    module: &mut M,
    ev: &'a mut ev::Ev,
    throttle: &'a mut throttle::Throttle,
    source: u8,
    time: u64,
    inputs: M::In<'a>,
    out: M::Out<'a>,
) {
    let mut ctx = ctx::Ctx::<M>::new(
        ev,
        throttle,
        source,
        time,
        reset_info(3),
        inputs,
        out,
        cmd::Inbox::empty(),
    );
    module.init(&mut ctx);
}

/// The same for `step`.
fn run_step<'a, M: module::Module>(
    module: &mut M,
    ev: &'a mut ev::Ev,
    throttle: &'a mut throttle::Throttle,
    source: u8,
    time: u64,
    inputs: M::In<'a>,
    out: M::Out<'a>,
) {
    let mut ctx = ctx::Ctx::<M>::new(
        ev,
        throttle,
        source,
        time,
        reset_info(3),
        inputs,
        out,
        cmd::Inbox::empty(),
    );
    module.step(&mut ctx);
}

#[test]
fn a_module_sees_how_the_last_run_ended_through_ctx() {
    let (mut ev, mut throttle) = (ev::Ev::new(), throttle::Throttle::new());
    run_init(
        &mut Counter { steps: 0 },
        &mut ev,
        &mut throttle,
        7,
        100,
        (),
        (),
    );
    assert_eq!(ev.log.pop().unwrap().text.as_bytes(), b"started, boot 3");
}

#[test]
fn an_event_has_the_modules_source_the_time_of_the_call_and_the_codes_severity() {
    let (mut ev, mut throttle) = (ev::Ev::new(), throttle::Throttle::new());
    run_step(
        &mut Counter { steps: 0 },
        &mut ev,
        &mut throttle,
        7,
        2_500,
        (),
        (),
    );
    let event = ev.log.pop().unwrap();
    assert_eq!(event.source, 7);
    assert_eq!(event.monotonic_us, 2_500);
    assert_eq!(event.code, 1);
    assert_eq!(event.severity, event::Severity::Warn);
    assert_eq!(event.text.as_bytes(), b"step 1 at 2500");
}

#[test]
fn the_module_cannot_choose_its_source_the_executive_does() {
    // The same module, run as two different sources, sends events from each.
    let (mut ev, mut throttle) = (ev::Ev::new(), throttle::Throttle::new());
    run_init(
        &mut Counter { steps: 0 },
        &mut ev,
        &mut throttle,
        4,
        0,
        (),
        (),
    );
    run_init(
        &mut Counter { steps: 0 },
        &mut ev,
        &mut throttle,
        9,
        0,
        (),
        (),
    );
    assert_eq!(ev.log.pop().unwrap().source, 4);
    assert_eq!(ev.log.pop().unwrap().source, 9);
}

#[test]
fn every_event_in_one_call_has_the_same_time() {
    let (mut ev, mut throttle) = (ev::Ev::new(), throttle::Throttle::new());
    run_step(&mut Noisy, &mut ev, &mut throttle, 1, 42, (), ());
    assert!(ev.log.len() > 1);
    while let Some(event) = ev.log.pop() {
        assert_eq!(event.monotonic_us, 42);
    }
}

#[test]
fn monotonic_is_the_time_the_executive_gave() {
    let (mut ev, mut throttle) = (ev::Ev::new(), throttle::Throttle::new());
    let ctx = ctx::Ctx::<Counter>::new(
        &mut ev,
        &mut throttle,
        1,
        777,
        reset_info(1),
        (),
        (),
        cmd::Inbox::empty(),
    );
    assert_eq!(ctx.monotonic(), 777);
    assert_eq!(ctx.reset, reset_info(1));
}

#[test]
fn a_module_keeps_its_own_state_between_calls_and_each_call_has_its_own_time() {
    let (mut ev, mut throttle) = (ev::Ev::new(), throttle::Throttle::new());
    let mut module = Counter { steps: 0 };
    run_step(&mut module, &mut ev, &mut throttle, 1, 10_000, (), ());
    run_step(&mut module, &mut ev, &mut throttle, 1, 20_000, (), ());
    assert_eq!(ev.log.pop().unwrap().text.as_bytes(), b"step 1 at 10000");
    assert_eq!(ev.log.pop().unwrap().text.as_bytes(), b"step 2 at 20000");
}

#[test]
fn an_event_sent_too_often_in_a_tick_is_not_queued_and_is_counted() {
    let (mut ev, mut throttle) = (ev::Ev::new(), throttle::Throttle::new());
    run_step(&mut Noisy, &mut ev, &mut throttle, 1, 0, (), ());

    // `LIMIT` spam events and the one quiet event got through.
    let mut spam = 0;
    let mut quiet = 0;
    while let Some(event) = ev.log.pop() {
        match event.code {
            0 => spam += 1,
            1 => quiet += 1,
            _ => panic!("unexpected code"),
        }
    }
    assert_eq!(spam, LIMIT);
    assert_eq!(quiet, 1);

    // The throttle knows how many were dropped, for the report at the start of the next tick.
    let mut reports = 0;
    throttle.new_tick(|code: NoisyEv, suppressed| {
        assert_eq!(code, NoisyEv::Spam);
        assert_eq!(suppressed, 5);
        reports += 1;
    });
    assert_eq!(reports, 1);
}

#[test]
fn the_limit_is_available_again_in_the_next_tick() {
    let (mut ev, mut throttle) = (ev::Ev::new(), throttle::Throttle::new());
    run_step(&mut Noisy, &mut ev, &mut throttle, 1, 0, (), ());
    throttle.new_tick(|_: NoisyEv, _| {});
    while ev.log.pop().is_some() {}
    run_step(&mut Noisy, &mut ev, &mut throttle, 1, 10_000, (), ());
    assert_eq!(ev.log.len(), usize::from(LIMIT) + 1);
}

#[test]
fn a_noisy_module_does_not_throttle_another_modules_events() {
    // Each module has its own throttle, so Counter's first step is not held back by Noisy.
    let mut ev = ev::Ev::new();
    let (mut noisy_throttle, mut counter_throttle) =
        (throttle::Throttle::new(), throttle::Throttle::new());
    run_step(&mut Noisy, &mut ev, &mut noisy_throttle, 1, 0, (), ());
    run_step(
        &mut Counter { steps: 0 },
        &mut ev,
        &mut counter_throttle,
        2,
        0,
        (),
        (),
    );
    let mut from_counter = 0;
    while let Some(event) = ev.log.pop() {
        if event.source == 2 {
            from_counter += 1;
        }
    }
    assert_eq!(from_counter, 1);
}

// ---- A module that handles commands (`ctx.drain_cmds`) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum RunnerEv {
    #[severity(Info)]
    Ran,
}

/// What a `Runner` command does, by its number: 0 fails, 1 is pending, anything else succeeds.
struct Runner;

/// How many commands a test queue holds: more than the throttle's limit for one event code.
const CMD_DEPTH: usize = LIMIT as usize + 3;

type CmdQueue = queue::Queue<(u8, cmd::CmdMeta), CMD_DEPTH>;

impl module::Module for Runner {
    type Event = RunnerEv;
    type In<'a> = ();
    type Out<'a> = ();
    type Cmd = u8;

    fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}

    fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
        ctx.drain_cmds(|command, ctx| {
            // The handler gets `ctx`: it can send the module's own events.
            ctx.ev.send(RunnerEv::Ran, format_args!("ran {command}"));
            match command {
                0 => cmd::CmdResult::Fail("zero"),
                1 => cmd::CmdResult::Pending,
                _ => cmd::CmdResult::Success,
            }
        });
    }
}

fn meta(seq: u16) -> cmd::CmdMeta {
    cmd::CmdMeta {
        sink: 5,
        seq,
        target: 7,
        opcode: 0x20,
    }
}

/// One `step` of a `Runner` on source `source` at `time`, with `queue` as its command queue.
fn step_runner(queue: &mut CmdQueue, ev: &mut ev::Ev, source: u8, time: u64) {
    let mut throttle = throttle::Throttle::new();
    let mut ctx = ctx::Ctx::<Runner>::new(
        ev,
        &mut throttle,
        source,
        time,
        reset_info(1),
        (),
        (),
        cmd::Inbox::new(queue),
    );
    module::Module::step(&mut Runner, &mut ctx);
}

/// Every event in the queue, oldest first, as (source, time, severity, text).
fn drained(ev: &mut ev::Ev) -> Vec<(u8, u64, event::Severity, String)> {
    let mut out = Vec::new();
    while let Some(event) = ev.log.pop() {
        out.push((
            event.source,
            event.monotonic_us,
            event.severity,
            event.text.as_str().to_owned(),
        ));
    }
    out
}

#[test]
fn drain_cmds_runs_each_command_oldest_first_and_sends_each_outcome() {
    let mut queue = CmdQueue::new();
    queue.push((9, meta(1))).unwrap();
    queue.push((8, meta(2))).unwrap();
    let mut ev = ev::Ev::new();
    step_runner(&mut queue, &mut ev, 4, 1000);

    let texts: Vec<_> = drained(&mut ev).into_iter().map(|e| e.3).collect();
    assert_eq!(
        texts,
        [
            "ran 9",
            "cmd 5/1 t7 op0x0020: succeeded",
            "ran 8",
            "cmd 5/2 t7 op0x0020: succeeded",
        ]
    );
    assert!(queue.is_empty());
}

#[test]
fn drain_cmds_reports_success_failure_and_pending_each_as_it_is() {
    let mut queue = CmdQueue::new();
    queue.push((2, meta(1))).unwrap();
    queue.push((0, meta(2))).unwrap();
    queue.push((1, meta(3))).unwrap();
    let mut ev = ev::Ev::new();
    step_runner(&mut queue, &mut ev, 4, 1000);

    let outcomes: Vec<_> = drained(&mut ev)
        .into_iter()
        .map(|e| e.3)
        .filter(|text| text.starts_with("cmd "))
        .collect();
    assert_eq!(
        outcomes,
        [
            "cmd 5/1 t7 op0x0020: succeeded",
            "cmd 5/2 t7 op0x0020: failed: zero",
            "cmd 5/3 t7 op0x0020: pending",
        ]
    );
}

#[test]
fn the_outcome_has_the_modules_source_and_the_calls_time() {
    let mut queue = CmdQueue::new();
    queue.push((2, meta(1))).unwrap();
    let mut ev = ev::Ev::new();
    step_runner(&mut queue, &mut ev, 4, 1234);

    let outcome = drained(&mut ev).pop().unwrap();
    assert_eq!((outcome.0, outcome.1), (4, 1234));
}

#[test]
fn a_failure_is_an_error_event_and_the_others_are_info() {
    let mut queue = CmdQueue::new();
    queue.push((2, meta(1))).unwrap();
    queue.push((0, meta(2))).unwrap();
    queue.push((1, meta(3))).unwrap();
    let mut ev = ev::Ev::new();
    step_runner(&mut queue, &mut ev, 4, 0);

    let severities: Vec<_> = drained(&mut ev)
        .into_iter()
        .filter(|e| e.3.starts_with("cmd "))
        .map(|e| e.2)
        .collect();
    assert_eq!(
        severities,
        [
            event::Severity::Info,
            event::Severity::Err,
            event::Severity::Info
        ]
    );
}

#[test]
fn outcomes_are_not_throttled_even_when_the_modules_own_events_are() {
    // `CMD_DEPTH` commands in one step: more than the throttle lets one event code through. The
    // handler's own `Ran` event is cut off at the limit; every outcome is still there.
    assert!(CMD_DEPTH > usize::from(LIMIT));
    let mut queue = CmdQueue::new();
    for seq in 0..CMD_DEPTH {
        queue.push((2, meta(u16::try_from(seq).unwrap()))).unwrap();
    }
    let mut ev = ev::Ev::new();
    step_runner(&mut queue, &mut ev, 4, 0);

    let texts: Vec<_> = drained(&mut ev).into_iter().map(|e| e.3).collect();
    let outcomes = texts.iter().filter(|t| t.starts_with("cmd ")).count();
    let ran = texts.iter().filter(|t| t.starts_with("ran ")).count();
    assert_eq!(outcomes, CMD_DEPTH);
    assert_eq!(ran, usize::from(LIMIT));
}

#[test]
fn drain_cmds_with_nothing_queued_sends_nothing() {
    let mut queue = CmdQueue::new();
    let mut ev = ev::Ev::new();
    step_runner(&mut queue, &mut ev, 4, 0);
    assert!(ev.log.is_empty());
}

#[test]
fn a_module_with_no_commands_has_an_inbox_that_is_always_empty() {
    // `Noisy` has `type Cmd = ();`: `drain_cmds` on it does nothing, and never panics.
    struct Drainer;
    impl module::Module for Drainer {
        type Event = NoisyEv;
        type In<'a> = ();
        type Out<'a> = ();
        type Cmd = ();
        fn init(&mut self, _ctx: &mut ctx::Ctx<'_, Self>) {}
        fn step(&mut self, ctx: &mut ctx::Ctx<'_, Self>) {
            ctx.drain_cmds(|_, _| cmd::CmdResult::Success);
        }
    }
    let mut ev = ev::Ev::new();
    let mut throttle = throttle::Throttle::new();
    run_step(&mut Drainer, &mut ev, &mut throttle, 1, 0, (), ());
    assert!(ev.log.is_empty());
}

#[test]
fn commands_left_in_the_queue_by_a_module_that_never_drains_stay_there() {
    // `Counter` has no commands, but the queue is the executive's: a module that does not drain
    // does not lose what is in it.
    let mut queue = CmdQueue::new();
    queue.push((2, meta(1))).unwrap();
    let mut ev = ev::Ev::new();
    let mut throttle = throttle::Throttle::new();
    run_step(
        &mut Counter { steps: 0 },
        &mut ev,
        &mut throttle,
        1,
        0,
        (),
        (),
    );
    assert_eq!(queue.len(), 1);
}
