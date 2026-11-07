use std::cell::RefCell;

// main idea borrowed_mut from:
// https://blog.iany.me/2019/03/how-to-mock-time-in-rust-tests-and-cargo-gotchas-we-met/
// see also:
// https://docs.rs/mock_instant/latest/mock_instant/

// FIXME: This should be November 12 1955 06:38, of course
// (or maybe OCT 21, 2015 07:28)
// 8 November 2022 12:13 Berlin time

thread_local! {
    static MOCK_TIME: RefCell<i64> = const { RefCell::new(1667906008578) };
}

pub fn get_milliseconds_since_epoch() -> i64 {
    MOCK_TIME.with(|t| {
        let now = *t.borrow();
        #[cfg(test)]
        {
            MOCK_STEP.with(|step| *t.borrow_mut() += step.get());
            MOCK_READS.with(|reads| reads.set(reads.get() + 1));
        }
        now
    })
}

pub fn set_mock_time(time: i64) {
    MOCK_TIME.with(|cell| *cell.borrow_mut() = time);
}

#[cfg(test)]
thread_local! {
    static MOCK_STEP: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
    static MOCK_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn clock_reads() -> usize {
    MOCK_READS.with(std::cell::Cell::get)
}

/// Advance after every read, restoring the thread's clock even if a test panics.
#[cfg(test)]
pub(crate) fn with_advancing_time<T>(start: i64, step: i64, test: impl FnOnce() -> T) -> T {
    struct Restore(i64, i64, usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            set_mock_time(self.0);
            MOCK_STEP.with(|step| step.set(self.1));
            MOCK_READS.with(|reads| reads.set(self.2));
        }
    }
    let _restore = Restore(
        MOCK_TIME.with(|time| *time.borrow()),
        MOCK_STEP.with(|value| value.replace(step)),
        MOCK_READS.with(|value| value.replace(0)),
    );
    set_mock_time(start);
    test()
}

#[cfg(test)]
mod tests {
    use crate::mock_time::MOCK_TIME;

    use super::get_milliseconds_since_epoch;

    #[test]
    fn mock_time() {
        let t = get_milliseconds_since_epoch();
        assert_eq!(t, 1667906008578);

        MOCK_TIME.with_borrow(|v| assert_eq!(*v, 1667906008578));
    }
}
