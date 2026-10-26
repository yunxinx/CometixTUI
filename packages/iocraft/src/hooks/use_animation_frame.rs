use crate::{Hook, Hooks};
use core::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use futures_timer::Delay;
use std::time::Duration;

use super::{TerminalViewportEntry, UseContext, UseState, UseTerminalViewport};
use crate::components::Clock;

mod private {
    pub trait Sealed {}
    impl Sealed for crate::Hooks<'_, '_> {}
}

/// Return value of [`UseAnimationFrame::use_animation_frame`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationFrameState {
    /// Current terminal viewport entry for the component using the hook.
    pub viewport: TerminalViewportEntry,
    /// Milliseconds elapsed on the shared animation clock.
    pub time_ms: u128,
}

/// Hook for synchronized, pausable animations.
///
/// This follows CC Ink's `useAnimationFrame(...)`: animations stop ticking
/// when their component is outside the live terminal viewport, and "the clock
/// automatically slows when the terminal is blurred, so consumers don't need
/// to handle focus state" — the render root halves the [`Clock`]'s base tick.
pub trait UseAnimationFrame: private::Sealed {
    /// Returns the current animation state, ticking at `interval` while active.
    fn use_animation_frame(&mut self, interval: Option<Duration>) -> AnimationFrameState;

    /// Calls `callback` every [`Clock::period`]`(interval)` on the tree's
    /// [`Clock`] grid (see [`Clock::delay_to_next`]): every animation with
    /// the same interval ticks at the same instants and shares a frame, as
    /// subscribers of CC Ink's single `ClockContext` do, and the interval is
    /// quantized to the clock's base tick as theirs is. `use_animation_frame`
    /// is built on it. Use [`use_interval`](super::UseInterval::use_interval)
    /// for timers whose phase should count from the moment they start.
    ///
    /// Passing `None` pauses the timer. Without a clock in context (CC:
    /// `clock` is `null`) the timer never ticks.
    fn use_animation_interval<F>(&mut self, callback: F, interval: Option<Duration>)
    where
        F: FnMut() + Send + Unpin + 'static;
}

impl UseAnimationFrame for Hooks<'_, '_> {
    fn use_animation_frame(&mut self, interval: Option<Duration>) -> AnimationFrameState {
        // The render root provides the clock (Ink's root App mounts
        // ClockProvider); CC reads `clock?.now() ?? 0` when there is none.
        let clock = self.try_use_context::<Clock>().map(|clock| clock.clone());
        let viewport = self.use_terminal_viewport();
        let active_interval = if viewport.is_visible { interval } else { None };
        let time = {
            let clock = clock.clone();
            self.use_state(move || clock.as_ref().map_or(0, Clock::now_ms))
        };
        let mut time_for_callback = time;
        self.use_animation_interval(
            move || {
                if let Some(clock) = &clock {
                    time_for_callback.set(clock.now_ms());
                }
            },
            active_interval,
        );
        AnimationFrameState {
            viewport,
            time_ms: time.get(),
        }
    }

    fn use_animation_interval<F>(&mut self, callback: F, interval: Option<Duration>)
    where
        F: FnMut() + Send + Unpin + 'static,
    {
        let clock = self.try_use_context::<Clock>().map(|clock| clock.clone());
        let hook = self.use_hook(UseAnimationIntervalImpl::<F>::default);
        if hook.interval != interval {
            hook.delay = None;
        }
        hook.interval = interval;
        hook.clock = clock;
        hook.callback = Some(callback);
        // The `Delay` is armed in `poll_change`; a render that (re)starts the
        // timer must be followed by one poll (see `Hooks::request_poll`).
        if interval.is_some() && hook.clock.is_some() && hook.delay.is_none() {
            self.request_poll();
        }
    }
}

/// A timer whose ticks land on the tree clock's grid. Same contract as
/// `UseIntervalImpl` — a tick runs the callback and stays `Pending`, since
/// only a state write is a visual change — but every `Delay` is armed to
/// the next grid line rather than to `interval` from now.
struct UseAnimationIntervalImpl<F> {
    callback: Option<F>,
    interval: Option<Duration>,
    clock: Option<Clock>,
    delay: Option<Pin<Box<Delay>>>,
}

impl<F> Default for UseAnimationIntervalImpl<F> {
    fn default() -> Self {
        Self {
            callback: None,
            interval: None,
            clock: None,
            delay: None,
        }
    }
}

impl<F> Hook for UseAnimationIntervalImpl<F>
where
    F: FnMut() + Send + Unpin,
{
    fn poll_change(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let (Some(interval), Some(clock)) = (self.interval, self.clock.clone()) else {
            self.delay = None;
            return Poll::Pending;
        };
        if self.delay.is_none() {
            self.delay = Some(Box::pin(Delay::new(clock.delay_to_next(interval))));
        }
        loop {
            let ready = self
                .delay
                .as_mut()
                .is_some_and(|delay| delay.as_mut().poll(cx).is_ready());
            if !ready {
                return Poll::Pending;
            }
            self.delay = Some(Box::pin(Delay::new(clock.delay_to_next(interval))));
            if let Some(callback) = self.callback.as_mut() {
                callback();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::*;
    use futures::StreamExt;

    #[component]
    fn OffscreenAnimation(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let mut tick = hooks.use_state(|| 0u8);
        if tick.get() < 2 {
            tick += 1;
        } else {
            system.exit();
        }

        let frame = hooks.use_animation_frame(Some(Duration::from_millis(1)));
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: "row 0")
                Text(content: "row 1")
                Text(content: "row 2")
                Text(content: "row 3")
                Text(content: format!(
                    "visible={} time={}",
                    frame.viewport.is_visible,
                    frame.time_ms,
                ))
            }
        }
    }

    #[test]
    fn test_use_animation_frame_reports_viewport_state() {
        let canvases: Vec<_> = smol::block_on(
            element!(OffscreenAnimation)
                .mock_terminal_render_loop(MockTerminalConfig::default().with_size(20, 3))
                .collect(),
        );
        assert!(
            canvases
                .last()
                .unwrap()
                .to_string()
                .contains("visible=true"),
            "component itself is the root and remains visible"
        );
    }

    #[component]
    fn LateClockChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut system = hooks.use_context_mut::<SystemContext>();
        let frame = hooks.use_animation_frame(None);
        system.exit();
        element!(Text(content: format!("late={}", frame.time_ms)))
    }

    #[component]
    fn SharedClockApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let frame = hooks.use_animation_frame(Some(Duration::from_millis(1)));
        element! {
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("parent={}", frame.time_ms))
                #(if frame.time_ms > 0 {
                    element!(LateClockChild).into_any()
                } else {
                    element!(Text(content: "waiting")).into_any()
                })
            }
        }
    }

    #[test]
    fn test_use_animation_frame_uses_shared_clock_for_late_mounts() {
        let canvases: Vec<_> = smol::block_on(
            element!(SharedClockApp)
                .mock_terminal_render_loop(MockTerminalConfig::default())
                .collect(),
        );
        let rendered = canvases.last().unwrap().to_string();
        let late = rendered
            .lines()
            .find_map(|line| line.strip_prefix("late="))
            .and_then(|value| value.parse::<u128>().ok())
            .expect("late-mounted child should render its clock time");
        assert!(
            late > 0,
            "late-mounted animations should inherit the shared clock instead of restarting at 0: {rendered:?}"
        );
    }

    #[component]
    fn TwoAnimations(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
        let mut show_late = hooks.use_state(|| false);
        hooks.use_terminal_events(move |event| {
            if let TerminalEvent::Key(_) = event {
                show_late.set(true);
            }
        });
        element! {
            View(flex_direction: FlexDirection::Column) {
                GridAnimation(label: "early")
                #(if show_late.get() {
                    element!(GridAnimation(label: "late")).into_any()
                } else {
                    element!(Text(content: "waiting")).into_any()
                })
            }
        }
    }

    #[derive(Default, Props)]
    struct GridAnimationProps {
        label: &'static str,
    }

    #[component]
    fn GridAnimation(
        mut hooks: Hooks,
        props: &GridAnimationProps,
    ) -> impl Into<AnyElement<'static>> {
        // 500ms quantizes to 512ms on the shared clock's 16ms base tick.
        let frame = hooks.use_animation_frame(Some(Duration::from_millis(500)));
        let mut ticks = hooks.use_ref(|| 0u32);
        let mut last_time = hooks.use_ref(|| frame.time_ms);
        if frame.time_ms != last_time.get() {
            last_time.set(frame.time_ms);
            ticks.set(ticks.get() + 1);
        }
        element!(Text(content: format!("{}={}", props.label, ticks.get())))
    }

    #[test]
    fn test_equal_intervals_tick_once_per_period_on_the_grid() {
        smol::block_on(async {
            let clock = Clock::new();
            let (events, input) = futures::channel::mpsc::unbounded();
            let mut app = element! {
                ContextProvider(value: crate::Context::owned(clock.clone())) {
                    TwoAnimations
                }
            };
            let mut output = app.mock_terminal_render_loop(MockTerminalConfig::with_events(input));
            let rendered = std::cell::RefCell::new(String::new());
            let executor = smol::LocalExecutor::new();
            let _render = executor.spawn(async {
                while let Some(canvas) = output.next().await {
                    *rendered.borrow_mut() = canvas.to_string();
                }
                panic!("render loop should remain active");
            });

            // Drive the real render loop only at these checkpoints. Mounting
            // halfway through the period separates grid arming (512ms) from
            // start-relative arming (768ms). Observe at their midpoint, with
            // 128ms on either side for timer delivery and test scheduling.
            //
            // Withhold polling across 1024ms until 1280ms, then check at 1664ms:
            // grid rearming targets 1536ms, start-relative rearming 1792ms.
            // The intervening probes must not tick, catching base-tick timers
            // or duplicate updates between grid lines. No callback timestamp
            // is required to land on the grid itself.
            // Each observation must finish before the next relevant grid
            // line or the earliest incorrect timer. Initialization and the
            // negative probes do not need a separate per-render time limit.
            for (at, before, expected) in [
                (0u128, 512, "early=0\nwaiting\n"),
                (256, 512, "early=0\nlate=0\n"),
                (640, 768, "early=1\nlate=1\n"),
                (768, 1024, "early=1\nlate=1\n"),
                (1280, 1536, "early=2\nlate=2\n"),
                (1408, 1536, "early=2\nlate=2\n"),
                (1664, 1792, "early=3\nlate=3\n"),
            ] {
                smol::Timer::after(Duration::from_millis(
                    u64::try_from(at.saturating_sub(clock.now_ms())).unwrap(),
                ))
                .await;
                if at == 256 {
                    events
                        .unbounded_send(TerminalEvent::Key(KeyEvent::new(
                            KeyEventKind::Press,
                            KeyCode::Enter,
                        )))
                        .unwrap();
                }
                // Honor wakes raised while polling (e.g. a timer writing
                // state) before reading the canvas, but do not await timers.
                while executor.try_tick() {}
                let observed_at = clock.now_ms();
                assert!(
                    observed_at < before,
                    "missed the {at}ms observation window before the {before}ms grid/timer boundary: polled at {observed_at}ms"
                );
                assert_eq!(
                    rendered.borrow().as_str(),
                    expected,
                    "at {observed_at}ms (target {at}ms)"
                );
            }
        });
    }
}
