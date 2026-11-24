//! Tests of `Ctx` and `Module`, playing the part of the executive: build a `Ctx` for each call,
//! call the module, and look at what ended up in the event queue. (They are here, not in
//! `ctx.rs`, so they can use `#[derive(event::Code)]`.)

use vee_core_api::ctx;
use vee_core_api::ev;
use vee_core_api::event;
use vee_core_api::module;
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
    let mut ctx = ctx::Ctx::<M>::new(ev, throttle, source, time, reset_info(3), inputs, out);
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
    let mut ctx = ctx::Ctx::<M>::new(ev, throttle, source, time, reset_info(3), inputs, out);
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
    let ctx = ctx::Ctx::<Counter>::new(&mut ev, &mut throttle, 1, 777, reset_info(1), (), ());
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
