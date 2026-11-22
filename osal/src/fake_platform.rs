//! The `Platform` for tests: a fake file system and a fake watchdog. Public (not test-only)
//! because `vee-core`'s tests use it too.

use crate::fake_files;
use crate::fake_watchdog;
use crate::files;
use crate::platform;
use core::marker::PhantomData;

/// A platform with the fake watchdog and the file system `F`, which is `FakeFiles` unless a test
/// needs a file system that behaves differently (one that fails, say). `Core::<FakePlatform>` is
/// the usual one, and `Core::<FakePlatform<FailingFiles>>` the special one.
///
/// It is never made, only named: it exists to be a type argument. `PhantomData<F>` is how a
/// struct says it is about `F` without holding one, which Rust requires for an unused parameter.
pub struct FakePlatform<F = fake_files::FakeFiles>(PhantomData<F>);

impl<F: files::Files + Clone + Send + 'static> platform::Platform for FakePlatform<F> {
    type Files = F;
    type Watchdog = fake_watchdog::FakeWatchdog;
}
