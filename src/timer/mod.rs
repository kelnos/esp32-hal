//! Integrated timer control
//!
//!
//!
//!
//!

use embedded_hal::timer::{Cancel, CountDown, Periodic};

use crate::clock_control::ClockControlConfig;
use crate::prelude::*;
use core::marker::PhantomData;
use esp32::{TIMG0, TIMG1};

pub mod watchdog;

/// Timer errors
#[derive(Debug)]
pub enum Error {
    /// Unsupported frequency configuration
    UnsupportedWatchdogConfig,
    /// Value out of range
    OutOfRange,
    Disabled,
}

/// Hardware timers
pub struct Timer<TIMG: TimerGroup, INST: TimerInst> {
    clock_control_config: ClockControlConfig,
    timg: *const esp32::timg::RegisterBlock,
    _group: PhantomData<TIMG>,
    _timer: PhantomData<INST>,
}

unsafe impl<TIMG: TimerGroup, INST: TimerInst> Send for Timer<TIMG, INST> {}

/// Interrupt events
pub enum Event {
    /// Timer timed out / count down ended
    TimeOut,
    /// Timer timed out / count down ended (Edge Interrupt)
    TimeOutEdge,
}

#[doc(hidden)]
pub trait TimerGroup: core::ops::Deref {}
impl TimerGroup for esp32::TIMG0 {}
impl TimerGroup for esp32::TIMG1 {}

#[doc(hidden)]
pub trait TimerInst {}
#[doc(hidden)]
pub struct Timer0 {}
impl TimerInst for Timer0 {}
#[doc(hidden)]
pub struct Timer1 {}
impl TimerInst for Timer1 {}

impl<TIMG: TimerGroup> Timer<TIMG, Timer0> {
    /// Create new timer resources
    ///
    /// This function will create 2 timers and 1 watchdog for a timer group.
    /// It uses the clock_control_config for obtaining the clock configuration.
    ///
    /// *Note: time to clock tick conversions are done with the clock frequency when the
    /// [start](embedded_hal::timer::CountDown::start) function is called. The clock frequency is not locked.*
    pub fn new(
        timg: TIMG,
        clock_control_config: ClockControlConfig,
    ) -> (
        Timer<TIMG, Timer0>,
        Timer<TIMG, Timer1>,
        watchdog::Watchdog<TIMG>,
    ) {
        let timer0 = Timer::<TIMG, Timer0> {
            clock_control_config,
            timg: &*timg as *const _ as *const esp32::timg::RegisterBlock,
            _group: PhantomData {},
            _timer: PhantomData {},
        };
        let timer1 = Timer::<TIMG, Timer1> {
            clock_control_config,
            timg: &*timg as *const _ as *const esp32::timg::RegisterBlock,
            _group: PhantomData {},
            _timer: PhantomData {},
        };

        (
            timer0,
            timer1,
            watchdog::Watchdog::new(timg, clock_control_config),
        )
    }
}

impl<INST: TimerInst> Timer<TIMG0, INST> {
    /// Releases the timer resources. Requires to release all timers and watchdog belonging to
    /// the same group at once.
    pub fn release(_timer0: Timer<TIMG0, Timer0>, _timer1: Timer<TIMG0, Timer1>) -> TIMG0 {
        unsafe { esp32::Peripherals::steal().TIMG0 }
    }
}

impl<INST: TimerInst> Timer<TIMG1, INST> {
    /// Releases the timer resources. Requires to release all timers and watchdog belonging to
    /// the same group at once.
    pub fn release(_timer0: Timer<TIMG1, Timer0>, _timer1: Timer<TIMG1, Timer1>) -> TIMG1 {
        unsafe { esp32::Peripherals::steal().TIMG1 }
    }
}

static TIMER_MUTEX: spin::Mutex<()> = spin::Mutex::new(());

macro_rules! timer {
    ($TIMX:ident, $INT_ENA:ident, $CONFIG:ident, $HI:ident, $LO: ident,
        $LOAD: ident, $LOAD_HI: ident, $LOAD_LO:ident, $UPDATE:ident, $ALARM_HI:ident,
        $ALARM_LO:ident, $EN:ident, $INCREASE:ident, $AUTO_RELOAD:ident, $DIVIDER:ident,
        $EDGE_INT_EN:ident, $LEVEL_INT_EN:ident, $ALARM_EN:ident,
        $INT_RAW:ident, $INT_ST:ident, $INT_CLR:ident
    ) => {
        impl<TIMG: TimerGroup> Timer<TIMG, $TIMX> {
            /// Starts listening for an `event`
            //  Needs multi-threaded protection as timer0 and 1 use same register
            pub fn listen(&mut self, event: Event) {
                match event {
                    Event::TimeOut => self.enable_level_interrupt(true),
                    Event::TimeOutEdge => self.enable_edge_interrupt(true),
                }
                unsafe {
                    interrupt::free(|_| {
                        TIMER_MUTEX.lock();
                        (*(self.timg))
                            .int_ena_timers
                            .modify(|_, w| w.$INT_ENA().set_bit());
                    })
                };
            }

            /// Stops listening for an `event`
            //  Needs multi-threaded protection as timer0 and 1 use same register
            pub fn unlisten(&mut self, event: Event) {
                match event {
                    Event::TimeOut => self.enable_level_interrupt(false),
                    Event::TimeOutEdge => self.enable_edge_interrupt(false),
                }
            }

            /// Clear interrupt once fired
            pub fn clear_interrupt(&mut self) {
                self.enable_alarm(true);
                unsafe {
                    (*(self.timg))
                        .int_clr_timers
                        .write(|w| w.$INT_CLR().set_bit())
                }
            }

            /// Set timer value
            pub fn set_value<T: Into<TicksU64>>(&mut self, value: T) {
                unsafe {
                    let timg = &*(self.timg);
                    let value: u64 = value.into().into();
                    timg.$LOAD_LO.write(|w| w.bits(value as u32));
                    timg.$LOAD_HI.write(|w| w.bits((value >> 32) as u32));
                    timg.$LOAD.write(|w| w.bits(1));
                }
            }

            /// Get timer value
            pub fn get_value(&mut self) -> TicksU64 {
                unsafe {
                    let timg = &*(self.timg);
                    timg.$UPDATE.write(|w| w.bits(1));
                    TicksU64(
                        ((timg.$HI.read().bits() as u64) << 32) | (timg.$LO.read().bits() as u64),
                    )
                }
            }

            /// Get alarm value
            pub fn get_alarm(&mut self) -> TicksU64 {
                unsafe {
                    let timg = &*(self.timg);
                    TicksU64(
                        ((timg.$ALARM_HI.read().bits() as u64) << 32)
                            | (timg.$ALARM_LO.read().bits() as u64),
                    )
                }
            }

            /// Set alarm value
            ///
            /// *Note: timer is not disabled, so there is a risk for false triggering between
            /// setting upper and lower 32 bits.*
            pub fn set_alarm(&mut self, value: TicksU64) {
                unsafe {
                    let timg = &*(self.timg);
                    let value: u64 = value.into();
                    timg.$ALARM_HI.write(|w| w.bits((value >> 32) as u32));
                    timg.$ALARM_LO.write(|w| w.bits(value as u32));
                }
            }

            /// Enable or disables the timer
            pub fn enable(&mut self, enable: bool) {
                unsafe { (*(self.timg)).$CONFIG.modify(|_, w| w.$EN().bit(enable)) }
            }

            /// Enable or disables the timer
            pub fn is_enabled(&mut self) -> bool {
                unsafe { (*(self.timg)).$CONFIG.read().$EN().bit_is_set() }
            }

            /// Stop the timer
            pub fn stop(&mut self) {
                self.enable(false);
            }

            /// Set to true to increase the timer value on each tick, set to false to decrease
            /// the timer value on each tick.
            pub fn increasing(&mut self, enable: bool) {
                unsafe {
                    (*(self.timg))
                        .$CONFIG
                        .modify(|_, w| w.$INCREASE().bit(enable))
                }
            }

            /// Returns true if teh timer is increasing, otherwise decreasing
            pub fn is_increasing(&mut self) -> bool {
                unsafe { (*(self.timg)).$CONFIG.read().$INCREASE().bit_is_set() }
            }

            /// Set to true if the timer needs to be reloaded to initial value once the alarm
            /// is reached.
            pub fn auto_reload(&mut self, enable: bool) {
                unsafe {
                    (*(self.timg))
                        .$CONFIG
                        .modify(|_, w| w.$AUTO_RELOAD().bit(enable))
                }
            }

            fn enable_edge_interrupt(&mut self, enable: bool) {
                unsafe {
                    (*(self.timg))
                        .$CONFIG
                        .modify(|_, w| w.$EDGE_INT_EN().bit(enable))
                }
            }

            fn enable_level_interrupt(&mut self, enable: bool) {
                unsafe {
                    (*(self.timg))
                        .$CONFIG
                        .modify(|_, w| w.$LEVEL_INT_EN().bit(enable))
                }
            }

            fn enable_alarm(&mut self, enable: bool) {
                unsafe {
                    (*(self.timg))
                        .$CONFIG
                        .modify(|_, w| w.$ALARM_EN().bit(enable))
                }
            }

            fn alarm_active(&mut self) -> bool {
                unsafe { (*(self.timg)).$CONFIG.read().$ALARM_EN().bit_is_set() }
            }

            /// Set clock divider.
            ///
            /// Value must be between 2 and 65536 inclusive.
            fn set_divider(&mut self, divider: u32) -> Result<(), Error> {
                if divider <= 1 || divider > 65536 {
                    return Err(Error::OutOfRange);
                }

                unsafe {
                    (*(self.timg)).$CONFIG.modify(|_, w| {
                        w.$DIVIDER()
                            .bits((if divider == 65536 { 0 } else { divider }) as u16)
                    })
                };

                Ok(())
            }
        }
        impl<TIMG: TimerGroup> Periodic for Timer<TIMG, $TIMX> {}

        impl<TIMG: TimerGroup> CountDown for Timer<TIMG, $TIMX> {
            type Time = NanoSecondsU64;

            /// Start timer
            fn start<T: Into<Self::Time>>(&mut self, timeout: T) {
                self.enable(false);
                self.set_divider(2).unwrap(); // minimum divider value is 2,
                                              // this still allows >14000years with 64 bit values

                self.auto_reload(true);
                self.set_value(0);
                self.set_alarm(self.clock_control_config.apb_frequency() / 2 * timeout.into());
                self.enable_alarm(true);
                self.enable(true);
            }

            /// Wait for timer to finish
            ///
            /// **Note: if the timeout is handled via an interrupt, this will never return.**
            fn wait(&mut self) -> nb::Result<(), void::Void> {
                if self.alarm_active() {
                    Err(nb::Error::WouldBlock)
                } else {
                    self.clear_interrupt();
                    Ok(())
                }
            }
        }

        impl<TIMG: TimerGroup> Cancel for Timer<TIMG, $TIMX> {
            type Error = Error;
            /// Cancel running timer.
            ///
            /// This will stop the timer if running and returns error when not running.
            fn cancel(&mut self) -> Result<(), Self::Error> {
                if !self.is_enabled() {
                    return Err(Self::Error::Disabled);
                }
                self.stop();
                Ok(())
            }
        }
    };
}

timer!(
    Timer0,
    t0_int_ena,
    t0config,
    t0hi,
    t0lo,
    t0load,
    t0loadhi,
    t0loadlo,
    t0update,
    t0alarmhi,
    t0alarmlo,
    t0_en,
    t0_increase,
    t0_autoreload,
    t0_divider,
    t0_edge_int_en,
    t0_level_int_en,
    t0_alarm_en,
    t0_int_raw,
    t0_int_st,
    t0_int_clr
);

timer!(
    Timer1,
    t1_int_ena,
    t1config,
    t1hi,
    t1lo,
    t1load,
    t1loadhi,
    t1loadlo,
    t1update,
    t1alarmhi,
    t1alarmlo,
    t1_en,
    t1_increase,
    t1_autoreload,
    t1_divider,
    t1_edge_int_en,
    t1_level_int_en,
    t1_alarm_en,
    t1_int_raw,
    t1_int_st,
    t1_int_clr
);
