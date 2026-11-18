//! Tests of `#[derive(event::Code)]` from outside the crate, as a real user sees it.
//! (Inside `core-api` itself the generated `::vee_core_api` paths would not resolve.)

use vee_core_api::event;
use vee_core_api::event::Code as _;
use vee_core_api::throttle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, event::Code)]
#[repr(u16)]
enum SampleEv {
    #[severity(Info)]
    Started,
    #[severity(Warn)]
    Slow,
    #[severity(Err)]
    OpenFailed,
    /// A value given on purpose pins the code, and the ones after it count on from it.
    #[severity(Info)]
    Pinned = 0x1000,
    #[severity(Err)]
    AfterPinned,
    #[severity(Err)]
    Largest = u16::MAX,
}

#[test]
fn a_code_with_no_value_counts_up_from_zero() {
    assert_eq!(SampleEv::Started.code(), 0);
    assert_eq!(SampleEv::Slow.code(), 1);
    assert_eq!(SampleEv::OpenFailed.code(), 2);
}

#[test]
fn a_given_value_is_the_code_and_the_next_counts_on_from_it() {
    assert_eq!(SampleEv::Pinned.code(), 0x1000);
    assert_eq!(SampleEv::AfterPinned.code(), 0x1001);
    assert_eq!(SampleEv::Largest.code(), u16::MAX);
}

#[test]
fn the_severity_is_the_one_on_the_variant() {
    assert_eq!(SampleEv::Started.severity(), event::Severity::Info);
    assert_eq!(SampleEv::Slow.severity(), event::Severity::Warn);
    assert_eq!(SampleEv::OpenFailed.severity(), event::Severity::Err);
    assert_eq!(SampleEv::Pinned.severity(), event::Severity::Info);
    assert_eq!(SampleEv::Largest.severity(), event::Severity::Err);
}

#[test]
fn a_code_can_be_used_through_the_trait_by_a_generic_function() {
    fn code_and_severity<C: event::Code>(code: &C) -> (u16, event::Severity) {
        (code.code(), code.severity())
    }
    assert_eq!(
        code_and_severity(&SampleEv::OpenFailed),
        (2, event::Severity::Err)
    );
}

#[test]
fn the_index_is_the_position_in_the_order_written_not_the_code() {
    assert_eq!(SampleEv::Started.index(), 0);
    assert_eq!(SampleEv::Slow.index(), 1);
    assert_eq!(SampleEv::OpenFailed.index(), 2);
    // Pinned has code 0x1000, but it is the fourth variant.
    assert_eq!(SampleEv::Pinned.index(), 3);
    assert_eq!(SampleEv::AfterPinned.index(), 4);
    assert_eq!(SampleEv::Largest.index(), 5);
}

#[test]
fn from_index_is_the_reverse_of_index_and_none_past_the_last_variant() {
    for event in [
        SampleEv::Started,
        SampleEv::Slow,
        SampleEv::OpenFailed,
        SampleEv::Pinned,
        SampleEv::AfterPinned,
        SampleEv::Largest,
    ] {
        assert_eq!(SampleEv::from_index(event.index()), Some(event));
    }
    assert_eq!(SampleEv::from_index(6), None);
    assert_eq!(SampleEv::from_index(usize::MAX), None);
}

#[test]
fn a_derived_enum_works_with_a_throttle() {
    let mut throttle = throttle::Throttle::new();
    let limit = throttle::EV_MAX_PER_TICK;
    for _ in 0..limit {
        assert_eq!(throttle.throttle(&SampleEv::Largest), None);
    }
    assert_eq!(throttle.throttle(&SampleEv::Largest), Some(1));
    // The last variant has the last counter, and the others are not touched.
    assert_eq!(throttle.throttle(&SampleEv::Started), None);

    let mut reports = 0;
    throttle.new_tick(|code: SampleEv, suppressed| {
        assert_eq!(code, SampleEv::Largest);
        assert_eq!(suppressed, 1);
        reports += 1;
    });
    assert_eq!(reports, 1);
}
