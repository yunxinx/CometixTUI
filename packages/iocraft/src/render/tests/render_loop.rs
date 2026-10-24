use super::super::*;
use crate::prelude::*;
use core::future::Future;
use macro_rules_attribute::apply;
use smol_macros::test;

#[derive(Default, Props)]
struct MyInnerComponentProps {
    label: String,
}

#[component]
fn MyInnerComponent(
    mut hooks: Hooks,
    props: &MyInnerComponentProps,
) -> impl Into<AnyElement<'static>> {
    // Count renders without scheduling one: a `State` bumped during render is
    // a render-phase update, which push-mode settle re-runs inside the same
    // frame (React rejects it outright). `Ref` records without waking.
    let mut counter = hooks.use_ref(|| 0);
    counter.set(counter.get() + 1);
    element! {
        Text(content: format!("render count ({}): {}", props.label, counter.get()))
    }
}

#[component]
fn MyComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0);

    hooks.use_future(async move {
        tick += 1;
    });

    if tick == 1 {
        system.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: format!("tick: {}", tick))
            MyInnerComponent(label: "a")
            #((0..2).map(|i| element! { MyInnerComponent(label: format!("b{}", i)) }))
            #((0..2).map(|i| element! { MyInnerComponent(key: i, label: format!("c{}", i)) }))
        }
    }
}

#[apply(test!)]
async fn test_terminal_render_loop() {
    let canvases: Vec<_> =
        mock_terminal_render_loop(&mut element!(MyComponent), MockTerminalConfig::default())
            .collect()
            .await;
    let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
    let expected = vec![
        "tick: 0\nrender count (a): 1\nrender count (b0): 1\nrender count (b1): 1\nrender count (c0): 1\nrender count (c1): 1\n",
        "tick: 1\nrender count (a): 2\nrender count (b0): 2\nrender count (b1): 2\nrender count (c0): 2\nrender count (c1): 2\n",
    ];
    assert_eq!(actual, expected);
}

#[derive(Default, Props)]
struct ContentSizeProbeProps;

#[derive(Default)]
struct ContentSizeProbe;

impl Component for ContentSizeProbe {
    type Props<'a> = ContentSizeProbeProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_layout_style(taffy::style::Style {
            size: taffy::Size {
                width: taffy::style::Dimension::length(10.0),
                height: taffy::style::Dimension::length(5.0),
            },
            padding: taffy::Rect {
                left: taffy::style::LengthPercentage::length(1.0),
                right: taffy::style::LengthPercentage::length(1.0),
                top: taffy::style::LengthPercentage::length(1.0),
                bottom: taffy::style::LengthPercentage::length(1.0),
            },
            border: taffy::Rect {
                left: taffy::style::LengthPercentage::length(1.0),
                right: taffy::style::LengthPercentage::length(1.0),
                top: taffy::style::LengthPercentage::length(1.0),
                bottom: taffy::style::LengthPercentage::length(1.0),
            },
            ..Default::default()
        });
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        let content = drawer.content_size();
        let visible = drawer.visible_size();
        let remaining = drawer.remaining_canvas_size();
        drawer.canvas().set_text(
            0,
            0,
            &format!("c{}v{}r{}", content.width, visible.width, remaining.width),
            CanvasTextStyle::default(),
        );
    }
}

#[test]
fn test_component_drawer_content_and_visible_size_match_ink_helpers() {
    let canvas = element!(ContentSizeProbe).render(Some(8));
    assert_eq!(canvas.to_string().lines().next(), Some("c4v8r8"));
}

/// Rows in each block of [`test_a_layout_taller_than_u16_rows_draws_every_row_in_place`]: three
/// of them reach past both `i16::MAX` and `u16::MAX`.
const TALL_BLOCK_ROWS: usize = 25_000;

#[test]
fn test_a_layout_taller_than_u16_rows_draws_every_row_in_place() {
    let block = |block: usize| {
        (0..TALL_BLOCK_ROWS)
            .map(|row| format!("{}", block * TALL_BLOCK_ROWS + row))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let canvas = element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: block(0))
            Text(content: block(1))
            Text(content: block(2))
        }
    }
    .render(Some(8));
    assert_eq!(canvas.height(), 3 * TALL_BLOCK_ROWS);
    for row in [0, 32_767, 32_768, 65_535, 65_536, 3 * TALL_BLOCK_ROWS - 1] {
        assert_eq!(canvas.get_text(0, row, 8, 1).trim_end(), row.to_string());
    }
}

#[component]
fn TallScrollBoxApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut frames = hooks.use_state(|| 0u8);
    if frames.get() == 2 {
        system.exit();
    } else {
        frames.set(frames.get() + 1);
    }
    let lines = (0..3 * TALL_BLOCK_ROWS)
        .map(|row| row.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    element! {
        View(width: 8, height: 2) {
            ScrollBox(sticky_scroll: true) {
                Text(content: lines)
            }
        }
    }
}

#[apply(test!)]
async fn test_a_sticky_scroll_box_shows_the_end_of_content_taller_than_u16_rows() {
    let canvases: Vec<_> = element!(TallScrollBoxApp)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;
    let last = 3 * TALL_BLOCK_ROWS - 1;
    assert_eq!(
        canvases.last().unwrap().to_string(),
        format!("{}\n{last}\n", last - 1)
    );
}

#[derive(Default)]
struct RenderWakeCounter {
    renders: u32,
}

impl Hook for RenderWakeCounter {}

#[component]
fn ResizeWakeComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let counter = hooks.use_hook(RenderWakeCounter::default);
    counter.renders += 1;
    let renders = counter.renders;
    if renders >= 2 {
        system.exit();
    }
    element!(Text(content: format!("render: {renders}")))
}

#[apply(test!)]
async fn test_resize_event_wakes_render_loop_without_subscriber() {
    let canvases: Vec<_> = element!(ResizeWakeComponent)
        .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
            vec![TerminalEvent::Resize(100, 40)],
        )))
        .collect()
        .await;
    let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
    assert_eq!(actual, vec!["render: 1\n", "render: 2\n"]);
}

#[component]
fn StaticResizeComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let counter = hooks.use_hook(RenderWakeCounter::default);
    counter.renders += 1;
    if counter.renders >= 2 {
        system.exit();
    }
    element!(Text(content: "static"))
}

#[apply(test!)]
async fn test_resize_event_repaints_unchanged_canvas() {
    let canvases: Vec<_> = element!(StaticResizeComponent)
        .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
            vec![TerminalEvent::Resize(100, 40)],
        )))
        .collect()
        .await;
    let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
    assert_eq!(actual, vec!["static\n", "static\n"]);
}

#[component]
fn ResizeBurstComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let (width, _) = hooks.use_terminal_size();
    if width == 70 {
        system.exit();
    }
    element!(Text(content: format!("width: {width}")))
}

#[apply(test!)]
async fn test_resize_burst_paints_once_after_settling() {
    let canvases: Vec<_> = element!(ResizeBurstComponent)
        .mock_terminal_render_loop(MockTerminalConfig::with_events(futures::stream::iter(
            vec![
                TerminalEvent::Resize(100, 40),
                TerminalEvent::Resize(90, 40),
                TerminalEvent::Resize(70, 40),
            ],
        )))
        .collect()
        .await;
    let actual = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
    assert_eq!(actual.len(), 2, "{actual:?}");
    assert_eq!(actual[1], "width: 70\n");
}

#[component]
fn ResizeSubmitOwner(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let (mut value, mut submitted) = *hooks.use_context::<(State<String>, State<String>)>();
    hooks.use_propagated_terminal_events(move |event| {
        if matches!(event.event(), TerminalEvent::Key(key) if key.code == KeyCode::Enter && key.kind != KeyEventKind::Release)
        {
            submitted.set(value.to_string());
            value.set(String::new());
            event.stop_propagation();
        }
    });
    element!(View(width: 0, height: 0))
}

#[component]
fn ControlledResizeInput(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut value = hooks.use_state(|| "hi".to_string());
    let submitted = hooks.use_state(String::new);
    element! {
        ContextProvider(value: Context::owned((value, submitted))) {
            View(width: 20, flex_direction: FlexDirection::Column) {
                Text(content: format!("sent={submitted}"))
                ResizeSubmitOwner
                TextInput(has_focus: true, value: value.to_string(), on_change: move |text| value.set(text))
            }
        }
    }
}

#[apply(test!)]
async fn test_resize_commits_submit_before_next_input() {
    for push_wake in [true, false] {
        crate::render::wake::set_push_wake_for_tests(Some(push_wake));
        for throttle in [None, Some(Duration::from_secs(60))] {
            for input in [
                TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Char('x'))),
                TerminalEvent::Paste("x".to_string()),
            ] {
                // Ready events and a long throttle exercise the input interruption
                // deterministically, without sleeps or dependence on CI scheduling.
                let events = futures::stream::iter([
                    TerminalEvent::Resize(90, 25),
                    TerminalEvent::Key(KeyEvent::new(KeyEventKind::Press, KeyCode::Enter)),
                    input,
                    TerminalEvent::Key(KeyEvent {
                        kind: KeyEventKind::Press,
                        code: KeyCode::Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                    }),
                ]);
                let mut element = element!(ControlledResizeInput);
                let helper = element.helper();
                let (term, output) =
                    Terminal::mock(MockTerminalConfig::with_events(events).with_size(80, 24));
                let render_loop = async {
                    let mut tree = Tree::new(element.props_mut(), helper);
                    tree.terminal_render_loop(term, throttle, None)
                        .await
                        .unwrap();
                };
                let (_, canvases) = futures::join!(render_loop, output.collect::<Vec<_>>());
                let frames = canvases.iter().map(|c| c.to_string()).collect::<Vec<_>>();
                assert_eq!(frames.last().unwrap(), "sent=hi\nx \n", "{frames:?}");
            }
        }
    }
    crate::render::wake::set_push_wake_for_tests(None);
}

#[derive(Default, Props)]
struct AvailableHeightProbeProps;

#[derive(Default)]
struct AvailableHeightProbe;

impl Component for AvailableHeightProbe {
    type Props<'a> = AvailableHeightProbeProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_measure_func(Box::new(|_, available, _| {
            let width = match available.height {
                taffy::AvailableSpace::Definite(_) => 8.0,
                _ => 3.0,
            };
            taffy::Size { width, height: 1.0 }
        }));
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        let content = if drawer.size().width >= 8 {
            "definite"
        } else {
            "max"
        };
        drawer
            .canvas()
            .set_text(0, 0, content, CanvasTextStyle::default());
    }
}

#[component]
fn AvailableHeightApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    hooks.use_context_mut::<SystemContext>().exit();
    element!(AvailableHeightProbe)
}

#[apply(test!)]
async fn test_main_screen_layout_receives_terminal_height_without_clamping_canvas() {
    let canvases: Vec<_> = element!(AvailableHeightApp)
        .mock_terminal_render_loop(MockTerminalConfig::default().with_size(20, 7))
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(canvases[0].to_string().lines().next(), Some("definite"));
}

#[component]
fn TallStaticComponent(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    system.exit();
    element!(Text(content: "one\ntwo\nthree\nfour"))
}

#[apply(test!)]
async fn test_fullscreen_render_clamps_canvas_to_terminal_rows() {
    let canvases: Vec<_> = element!(TallStaticComponent)
        .mock_terminal_render_loop(
            MockTerminalConfig::default()
                .with_fullscreen(true)
                .with_size(10, 2),
        )
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(canvases[0].width(), 10);
    assert_eq!(canvases[0].height(), 2);
    assert_eq!(canvases[0].to_string(), "one\ntwo\n");
}

#[component]
fn ShortStaticComponent(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    system.exit();
    element!(Text(content: "one"))
}

#[apply(test!)]
async fn test_fullscreen_render_extends_canvas_to_terminal_rows() {
    let canvases: Vec<_> = element!(ShortStaticComponent)
        .mock_terminal_render_loop(
            MockTerminalConfig::default()
                .with_fullscreen(true)
                .with_size(10, 4),
        )
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(canvases[0].width(), 10);
    assert_eq!(canvases[0].height(), 4);
    assert_eq!(canvases[0].to_string(), "one\n\n\n\n");
}

struct PercentWidthProbe;

impl Component for PercentWidthProbe {
    type Props<'a> = crate::props::NoProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_layout_style(taffy::style::Style {
            size: taffy::geometry::Size {
                width: taffy::style::Dimension::percent(1.0),
                height: taffy::style::Dimension::length(1.0),
            },
            ..Default::default()
        });
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        let width = drawer.size().width.saturating_sub(1) as isize;
        drawer
            .canvas()
            .set_text(width, 0, "x", CanvasTextStyle::default());
    }
}

#[component]
fn PercentWidthProbeApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    system.exit();
    element!(PercentWidthProbe)
}

#[apply(test!)]
async fn test_fullscreen_layout_percent_width_resolves_against_terminal_columns() {
    let canvases: Vec<_> = element!(PercentWidthProbeApp)
        .mock_terminal_render_loop(
            MockTerminalConfig::default()
                .with_fullscreen(true)
                .with_size(10, 4),
        )
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(canvases[0].to_string(), "         x\n\n\n\n");
}

struct PercentHeightProbe;

impl Component for PercentHeightProbe {
    type Props<'a> = crate::props::NoProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_layout_style(taffy::style::Style {
            size: taffy::geometry::Size {
                width: taffy::style::Dimension::length(1.0),
                height: taffy::style::Dimension::percent(1.0),
            },
            ..Default::default()
        });
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        let y = drawer.size().height.saturating_sub(1) as isize;
        drawer
            .canvas()
            .set_text(0, y, "x", CanvasTextStyle::default());
    }
}

#[component]
fn PercentHeightProbeApp(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    system.exit();
    element!(PercentHeightProbe)
}

#[apply(test!)]
async fn test_fullscreen_layout_percent_height_resolves_against_terminal_rows() {
    let canvases: Vec<_> = element!(PercentHeightProbeApp)
        .mock_terminal_render_loop(
            MockTerminalConfig::default()
                .with_fullscreen(true)
                .with_size(10, 4),
        )
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(canvases[0].to_string(), "\n\n\nx\n");
}

#[apply(test!)]
async fn test_main_screen_render_does_not_clamp_canvas_to_terminal_rows() {
    let canvases: Vec<_> = element!(TallStaticComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default().with_size(10, 2))
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(canvases[0].height(), 4);
    assert_eq!(canvases[0].to_string(), "one\ntwo\nthree\nfour\n");
}

#[component]
fn LayoutShiftComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut expanded = hooks.use_state(|| false);

    hooks.use_future(async move {
        expanded.set(true);
    });

    if expanded.get() {
        system.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: if expanded.get() { "top\nextra" } else { "top" })
            Text(content: "bottom")
        }
    }
}

#[derive(Default)]
struct ForceFullRepaintOnSecondUpdateHook {
    updates: u32,
}

impl Hook for ForceFullRepaintOnSecondUpdateHook {
    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        self.updates += 1;
        if self.updates >= 2 {
            updater.force_full_repaint();
        }
    }
}

#[component]
fn ForceFullRepaintComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u32);
    let _ = hooks.use_hook(ForceFullRepaintOnSecondUpdateHook::default);

    hooks.use_future(async move {
        tick += 1;
    });

    if tick > 0 {
        system.exit();
    }

    element!(Text(content: "static"))
}

#[apply(test!)]
async fn test_component_updater_can_force_full_repaint_for_identical_canvas() {
    let canvases: Vec<_> = element!(ForceFullRepaintComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 2);
    assert_eq!(canvases[0].to_string(), "static\n");
    assert_eq!(canvases[1].to_string(), "static\n");
    assert!(!canvases[0].should_force_full_repaint());
    assert!(
        canvases[1].should_force_full_repaint(),
        "ComponentUpdater::force_full_repaint should be one-shot render metadata"
    );
}

#[derive(Default)]
struct InvalidatePrevFrameOnSecondUpdateHook {
    updates: u32,
}

impl Hook for InvalidatePrevFrameOnSecondUpdateHook {
    fn post_component_update(&mut self, updater: &mut ComponentUpdater) {
        self.updates += 1;
        if self.updates >= 2 {
            updater.invalidate_previous_frame();
        }
    }
}

#[component]
fn InvalidatePrevFrameComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u32);
    let _ = hooks.use_hook(InvalidatePrevFrameOnSecondUpdateHook::default);

    hooks.use_future(async move {
        tick += 1;
    });

    if tick > 0 {
        system.exit();
    }

    element!(Text(content: "static"))
}

#[apply(test!)]
async fn test_component_updater_invalidate_previous_frame_marks_full_damage() {
    let canvases: Vec<_> = element!(InvalidatePrevFrameComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 2);
    assert_eq!(canvases[0].to_string(), "static\n");
    assert_eq!(canvases[1].to_string(), "static\n");
    assert_eq!(canvases[0].damage_region(), None);
    assert_eq!(
        canvases[1].damage_region(),
        Some(DamageRegion {
            x: 0,
            y: 0,
            width: canvases[1].width(),
            height: canvases[1].height(),
        }),
        "invalidate_previous_frame should map to CC Ink-style full-screen damage"
    );
    assert!(
        !canvases[1].should_force_full_repaint(),
        "prev-frame invalidation should not disable scroll optimizations"
    );
}

#[derive(Default)]
struct MarkDamageOnSecondDrawHook {
    draws: u32,
}

impl Hook for MarkDamageOnSecondDrawHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.draws += 1;
        if self.draws >= 2 {
            drawer.mark_damage();
        }
    }
}

#[component]
fn DamageMarkerComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u32);
    let _ = hooks.use_hook(MarkDamageOnSecondDrawHook::default);

    hooks.use_future(async move {
        tick += 1;
    });

    if tick > 0 {
        system.exit();
    }

    element!(Text(content: "static"))
}

#[apply(test!)]
async fn test_component_drawer_damage_wakes_identical_canvas() {
    let canvases: Vec<_> = element!(DamageMarkerComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 2);
    assert_eq!(canvases[0].to_string(), "static\n");
    assert_eq!(canvases[1].to_string(), "static\n");
    assert_eq!(canvases[0].damage_region(), None);
    assert!(
        canvases[1].damage_region().is_some(),
        "ComponentDrawer::mark_damage should be one-shot render metadata that wakes the frame"
    );
}

#[derive(Default)]
struct MarkNoSelectHook;

impl Hook for MarkNoSelectHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        drawer.mark_no_select_region(1, 0, 2, 1);
    }
}

#[component]
fn NoSelectMarkerComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let _ = hooks.use_hook(MarkNoSelectHook::default);
    system.exit();
    element!(Text(content: "abcd"))
}

#[apply(test!)]
async fn test_component_drawer_no_select_marks_canvas_metadata() {
    let canvases: Vec<_> = element!(NoSelectMarkerComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(canvases[0].to_string(), "abcd\n");
    assert!(!canvases[0].is_no_select(0, 0));
    assert!(canvases[0].is_no_select(1, 0));
    assert!(canvases[0].is_no_select(2, 0));
    assert!(!canvases[0].is_no_select(3, 0));
    assert_eq!(
        canvases[0].damage_region(),
        None,
        "noSelect is selection metadata, not terminal-output damage"
    );
}

#[derive(Default)]
struct MarkDamageOnlyOnSecondDrawHook {
    draws: u32,
}

impl Hook for MarkDamageOnlyOnSecondDrawHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.draws += 1;
        if self.draws == 2 {
            drawer.mark_damage();
        }
    }
}

#[component]
fn PrevDamageCarryComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u32);
    let _ = hooks.use_hook(MarkDamageOnlyOnSecondDrawHook::default);

    hooks.use_future(async move {
        tick += 1;
        futures_timer::Delay::new(std::time::Duration::from_millis(1)).await;
        tick += 1;
    });

    if tick >= 2 {
        system.exit();
    }

    element!(Text(content: "static"))
}

#[apply(test!)]
async fn test_prev_damage_wakes_next_identical_render_once() {
    let canvases: Vec<_> = element!(PrevDamageCarryComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(
        canvases.len(),
        3,
        "previous-frame damage should force one cleanup diff even when the next canvas cells are identical"
    );
    assert_eq!(canvases[0].damage_region(), None);
    assert!(canvases[1].damage_region().is_some());
    assert_eq!(canvases[2].damage_region(), None);
    assert!(canvases
        .iter()
        .all(|canvas| canvas.to_string() == "static\n"));
}

#[derive(Default)]
struct MarkDamageRegionOnSecondDrawHook {
    draws: u32,
}

impl Hook for MarkDamageRegionOnSecondDrawHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.draws += 1;
        if self.draws >= 2 {
            drawer.mark_damage_region(1, 0, 2, 1);
        }
    }
}

#[component]
fn DamageRegionMarkerComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u32);
    let _ = hooks.use_hook(MarkDamageRegionOnSecondDrawHook::default);

    hooks.use_future(async move {
        tick += 1;
    });

    if tick > 0 {
        system.exit();
    }

    element!(Text(content: "static"))
}

#[apply(test!)]
async fn test_component_drawer_can_mark_local_damage_region() {
    let canvases: Vec<_> = element!(DamageRegionMarkerComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 2);
    assert_eq!(
        canvases[1].damage_region(),
        Some(DamageRegion {
            x: 1,
            y: 0,
            width: 2,
            height: 1,
        })
    );
}

#[derive(Default)]
struct MarkOutOfBoundsDamageRegionOnSecondDrawHook {
    draws: u32,
}

impl Hook for MarkOutOfBoundsDamageRegionOnSecondDrawHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        self.draws += 1;
        if self.draws >= 2 {
            drawer.mark_damage_region(5, 0, 10, 10);
        }
    }
}

#[component]
fn ClippedDamageRegionMarkerComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u32);
    let _ = hooks.use_hook(MarkOutOfBoundsDamageRegionOnSecondDrawHook::default);

    hooks.use_future(async move {
        tick += 1;
    });

    if tick > 0 {
        system.exit();
    }

    element!(Text(content: "static"))
}

#[apply(test!)]
async fn test_component_drawer_damage_region_clips_to_component_bounds() {
    let canvases: Vec<_> = element!(ClippedDamageRegionMarkerComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 2);
    assert_eq!(
        canvases[1].damage_region(),
        Some(DamageRegion {
            x: 5,
            y: 0,
            width: 1,
            height: 1,
        })
    );
}

#[component]
fn ClipRectDamageChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _ = hooks.use_hook(MarkDamageOnSecondDrawHook::default);
    element!(Text(content: "static"))
}

#[component]
fn ClipRectDamageParent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u32);

    hooks.use_future(async move {
        tick += 1;
    });

    if tick > 0 {
        system.exit();
    }

    element! {
        View(width: 4, overflow: Overflow::Hidden) {
            ClipRectDamageChild
        }
    }
}

#[apply(test!)]
async fn test_component_drawer_damage_region_clips_to_parent_clip_rect() {
    let canvases: Vec<_> = element!(ClipRectDamageParent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 2);
    assert_eq!(
        canvases[1].damage_region(),
        Some(DamageRegion {
            x: 0,
            y: 0,
            width: 4,
            height: 2,
        })
    );
}

#[derive(Default)]
struct ScrollHintOnDrawHook;

impl Hook for ScrollHintOnDrawHook {
    fn post_component_draw(&mut self, drawer: &mut ComponentDrawer) {
        drawer.set_scroll_hint(1);
    }
}

#[component]
fn ScrollHintChild(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let _ = hooks.use_hook(ScrollHintOnDrawHook::default);
    element! {
        View(width: 8, height: 4) {
            Text(content: "one\ntwo\nthree\nfour")
        }
    }
}

#[component]
fn ClippedScrollHintParent(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    system.exit();

    element! {
        View(width: 8, height: 2, overflow: Overflow::Hidden) {
            ScrollHintChild
        }
    }
}

#[apply(test!)]
async fn test_component_drawer_scroll_hint_clips_to_parent_clip_rect() {
    let canvases: Vec<_> = element!(ClippedScrollHintParent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(
        canvases[0].scroll_hint(),
        Some(ScrollHint {
            top: 0,
            bottom: 1,
            delta: 1,
        })
    );
}

struct PartialWidthScrollHintBox;

impl Component for PartialWidthScrollHintBox {
    type Props<'a> = crate::props::NoProps;

    fn new(_props: &Self::Props<'_>) -> Self {
        Self
    }

    fn update(
        &mut self,
        _props: &mut Self::Props<'_>,
        _hooks: Hooks,
        updater: &mut ComponentUpdater,
    ) {
        updater.set_layout_style(taffy::style::Style {
            size: taffy::geometry::Size {
                width: taffy::style::Dimension::length(4.0),
                height: taffy::style::Dimension::length(2.0),
            },
            ..Default::default()
        });
    }

    fn draw(&mut self, drawer: &mut ComponentDrawer<'_>) {
        drawer.set_scroll_hint(1);
        drawer
            .canvas()
            .set_text(0, 0, "box", CanvasTextStyle::default());
    }
}

#[component]
fn PartialWidthScrollHintParent(hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    system.exit();

    element! {
        View(width: 10, height: 2) {
            PartialWidthScrollHintBox
        }
    }
}

#[apply(test!)]
async fn test_component_drawer_scroll_hint_requires_full_canvas_width() {
    let canvases: Vec<_> = element!(PartialWidthScrollHintParent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 1);
    assert_eq!(
        canvases[0].scroll_hint(),
        None,
        "DECSTBM scroll regions move full terminal rows, so partial-width components must not emit hints"
    );
}

#[apply(test!)]
async fn test_main_screen_layout_shift_marks_next_canvas_damage() {
    let canvases: Vec<_> = element!(LayoutShiftComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    assert_eq!(canvases.len(), 2);
    assert_eq!(
        canvases[0].damage_region(),
        None,
        "first frame has no previous layout to compare"
    );
    assert_eq!(
        canvases[1].damage_region(),
        Some(DamageRegion {
            x: 0,
            y: 0,
            width: canvases[1].width(),
            height: canvases[1].height(),
        }),
        "CC Ink applies full-screen damage layout-shift backstop on main-screen too"
    );
    assert!(
        !canvases[1].should_force_full_repaint(),
        "layout shifts should use damage metadata rather than disabling scroll optimizations"
    );
}

// Shows an absolutely positioned overlay, then removes it; nothing else moves.
#[component]
fn OverlayToggleComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut step = hooks.use_state(|| 0u8);

    hooks.use_future(async move {
        for _ in 0..2 {
            smol::Timer::after(Duration::from_millis(20)).await;
            step.set(step.get() + 1);
        }
    });

    if step.get() == 2 {
        system.exit();
    }

    element! {
        View(flex_direction: FlexDirection::Column, width: 8) {
            Text(content: "one")
            Text(content: "two")
            #((step.get() == 1).then(|| element! {
                View(position: Position::Absolute, top: 0, left: 4) {
                    Text(content: "menu")
                }
            }))
        }
    }
}

#[apply(test!)]
async fn test_removed_drawn_node_marks_canvas_damage_but_added_node_does_not() {
    let canvases: Vec<_> = element!(OverlayToggleComponent)
        .mock_terminal_render_loop(MockTerminalConfig::default())
        .collect()
        .await;

    let frames: Vec<_> = canvases.iter().map(|canvas| canvas.to_string()).collect();
    assert_eq!(frames, ["one\ntwo\n", "one menu\ntwo\n", "one\ntwo\n"]);
    assert_eq!(
        canvases[1].damage_region(),
        None,
        "a node that appears moves nothing that was drawn"
    );
    assert_eq!(
        canvases[2].damage_region(),
        Some(DamageRegion {
            x: 0,
            y: 0,
            width: canvases[2].width(),
            height: canvases[2].height(),
        }),
        "CC Ink counts a removed child as a layout shift"
    );
}

/// A component that updates state rapidly (every poll) until 20 updates have
/// occurred. With throttling enabled, many updates coalesce into few frames.
#[component]
fn RapidComponent(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let mut tick = hooks.use_state(|| 0u32);

    hooks.use_future(async move {
        for _ in 0..20 {
            // Yield so each increment lands in a separate poll cycle, then bump.
            futures_timer::Delay::new(std::time::Duration::from_millis(1)).await;
            tick += 1;
        }
    });

    if tick >= 20 {
        system.exit();
    }

    element!(Text(content: format!("tick: {}", tick)))
}

#[test]
fn test_frame_profile_stats_accumulates_benchmark_metrics() {
    let mut stats = RenderFrameProfileStats::default();
    stats.record(&RenderFrameProfile {
        duration: Duration::from_millis(10),
        phases: RenderFramePhases {
            update: Duration::from_millis(1),
            layout: Duration::from_millis(2),
            draw: Duration::from_millis(3),
            changed_cell_scan: Duration::from_millis(4),
            repaint_check: Duration::from_millis(5),
            terminal_write: Duration::from_millis(6),
            diff_rows_scanned: 2,
            changed_cells: 5,
            canvas_width: 10,
            canvas_height: 2,
            layout_measures: 0,
            event_snapshot: Duration::ZERO,
            canvas_alloc: Duration::ZERO,
            canvas_swap: Duration::ZERO,
            settle_rounds: 0,
            layout_nodes: 0,
            sync_wrap: Duration::ZERO,
        },
        repaint: Some(DebugRepaintInfo {
            reason: DebugRepaintReason::FirstFrame,
            damage: None,
            previous_damage: None,
            changed_cells: 5,
            canvas_width: 10,
            canvas_height: 2,
        }),
    });
    stats.record(&RenderFrameProfile {
        duration: Duration::from_millis(30),
        phases: RenderFramePhases {
            update: Duration::from_millis(3),
            layout: Duration::from_millis(4),
            draw: Duration::from_millis(5),
            changed_cell_scan: Duration::from_millis(6),
            repaint_check: Duration::from_millis(7),
            terminal_write: Duration::from_millis(8),
            diff_rows_scanned: 4,
            changed_cells: 9,
            canvas_width: 10,
            canvas_height: 2,
            layout_measures: 0,
            event_snapshot: Duration::ZERO,
            canvas_alloc: Duration::ZERO,
            canvas_swap: Duration::ZERO,
            settle_rounds: 0,
            layout_nodes: 0,
            sync_wrap: Duration::ZERO,
        },
        repaint: None,
    });

    assert_eq!(stats.frames, 2);
    assert_eq!(stats.repaint_frames, 1);
    assert_eq!(stats.max_duration, Duration::from_millis(30));
    assert_eq!(stats.average_duration(), Duration::from_millis(20));
    assert_eq!(stats.average_update(), Duration::from_millis(2));
    assert_eq!(stats.average_layout(), Duration::from_millis(3));
    assert_eq!(stats.average_draw(), Duration::from_millis(4));
    assert_eq!(stats.average_changed_cell_scan(), Duration::from_millis(5));
    assert_eq!(stats.average_repaint_check(), Duration::from_millis(6));
    assert_eq!(stats.average_terminal_write(), Duration::from_millis(7));
    assert_eq!(stats.total_diff_rows_scanned, 6);
    assert_eq!(stats.max_diff_rows_scanned, 4);
    assert_eq!(stats.total_changed_cells, 14);
    assert_eq!(stats.max_changed_cells, 9);
    assert_eq!(stats.average_changed_cells(), 7.0);
    assert_eq!(stats.repaint_ratio(), 0.5);
}

#[apply(test!)]
async fn test_frame_profile_callback_reports_repaint_phases() {
    // These assertions depend on the profiling-only changed-cell scan.
    crate::render::set_changed_cell_count_for_tests(Some(true));
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let events_for_callback = events.clone();
    let mut element = element!(MyComponent);
    let h = element.helper();
    let (term, output) = Terminal::mock(MockTerminalConfig::default());
    let render_loop = async {
        let mut tree = Tree::new(element.props_mut(), h);
        tree.terminal_render_loop(
            term,
            None,
            Some(Box::new(move |event| {
                events_for_callback.lock().unwrap().push(event);
            })),
        )
        .await
        .unwrap();
    };
    let collect = output.collect::<Vec<_>>();
    let (_, canvases) = futures::join!(render_loop, collect);

    assert!(!canvases.is_empty());
    let events = events.lock().unwrap();
    assert_eq!(events.len(), canvases.len());
    let first = events
        .iter()
        .find(|event| event.repaint.is_some())
        .expect("at least the first frame should repaint");
    assert_eq!(
        first.repaint.as_ref().map(|repaint| repaint.reason),
        Some(DebugRepaintReason::FirstFrame)
    );
    assert!(first.phases.canvas_width > 0);
    assert!(first.phases.canvas_height > 0);
    assert!(
        first.phases.changed_cells > 0,
        "profile should include a retained-canvas change count"
    );
    assert!(
        first.phases.changed_cell_scan > Duration::ZERO,
        "profile should include changed-cell scan timing"
    );
    assert!(
        first.phases.repaint_check > Duration::ZERO,
        "profile should include repaint guard/equality timing"
    );
    assert_eq!(
        first.phases.diff_rows_scanned, 0,
        "baseline render loop does not opt into single-pass row planning"
    );
}

#[apply(test!)]
async fn test_mock_terminal_render_loop_with_single_pass_diff_planning_reports_rows() {
    let stats = std::sync::Arc::new(std::sync::Mutex::new(RenderFrameProfileStats::default()));
    let stats_for_callback = stats.clone();
    let canvases: Vec<_> = element!(MyComponent)
        .mock_terminal_render_loop_with_profile(
            MockTerminalConfig::default().with_canvas_diff_planning(
                TerminalDiffPlanning::SinglePass {
                    bounds: DiffBoundsMode::FullCanvas,
                    count_changed_cells: true,
                },
            ),
            move |event| {
                stats_for_callback.lock().unwrap().record(&event);
            },
        )
        .collect()
        .await;

    let stats = stats.lock().unwrap();
    assert_eq!(stats.frames, canvases.len());
    assert!(stats.repaint_frames > 0);
    assert!(
        stats.max_diff_rows_scanned > 0,
        "single-pass diff planning should report retained rows scanned"
    );
    assert!(
        stats.max_changed_cells > 0,
        "single-pass diff planning should preserve changed-cell profile counts"
    );
}

#[apply(test!)]
async fn test_mock_terminal_render_loop_with_profile_reports_events() {
    // These assertions depend on the profiling-only changed-cell scan.
    crate::render::set_changed_cell_count_for_tests(Some(true));
    let stats = std::sync::Arc::new(std::sync::Mutex::new(RenderFrameProfileStats::default()));
    let stats_for_callback = stats.clone();
    let canvases: Vec<_> = element!(MyComponent)
        .mock_terminal_render_loop_with_profile(MockTerminalConfig::default(), move |event| {
            stats_for_callback.lock().unwrap().record(&event);
        })
        .collect()
        .await;

    let stats = stats.lock().unwrap();
    assert_eq!(stats.frames, canvases.len());
    assert!(stats.frames > 0);
    assert!(stats.repaint_frames > 0);
    assert!(stats.max_changed_cells > 0);
}

#[apply(test!)]
async fn test_render_loop_throttling_coalesces_frames() {
    // Without throttling: ~one frame per tick (21 frames including the initial).
    let unthrottled: Vec<_> = {
        let mut element = element!(RapidComponent);
        let (term, output) = Terminal::mock(MockTerminalConfig::default());
        let mut h = element.helper();
        let render_loop = async {
            let mut tree = Tree::new(element.props_mut(), h);
            tree.terminal_render_loop(term, None, None).await.unwrap();
        };
        let collect = output.collect::<Vec<_>>();
        let (_, canvases) = futures::join!(render_loop, collect);
        h = element.helper();
        let _ = h;
        canvases
    };

    // With a 50ms throttle: the 20 ticks (at ~1ms apart) coalesce into far fewer
    // frames — at most a handful of throttle windows pass during the run.
    let throttled: Vec<_> = {
        let mut element = element!(RapidComponent);
        let h = element.helper();
        let (term, output) = Terminal::mock(MockTerminalConfig::default());
        let render_loop = async {
            let mut tree = Tree::new(element.props_mut(), h);
            tree.terminal_render_loop(term, Some(std::time::Duration::from_millis(50)), None)
                .await
                .unwrap();
        };
        let collect = output.collect::<Vec<_>>();
        let (_, canvases) = futures::join!(render_loop, collect);
        canvases
    };

    assert!(
        unthrottled.len() > throttled.len(),
        "throttling should reduce frame count: unthrottled={} throttled={}",
        unthrottled.len(),
        throttled.len()
    );
    // Conservative bound to avoid CI timing flakiness: 20 ticks at 1ms within
    // 50ms windows should need well under half the unthrottled frame count.
    assert!(
        throttled.len() <= unthrottled.len() / 2,
        "expected at most half the frames: unthrottled={} throttled={}",
        unthrottled.len(),
        throttled.len()
    );
    // Both runs must end on the final state.
    assert!(throttled.last().unwrap().to_string().contains("tick: 20"));
    assert!(unthrottled.last().unwrap().to_string().contains("tick: 20"));
}

async fn await_send_future<F: Future<Output = io::Result<()>> + Send>(f: F) {
    f.await.unwrap();
}

// Make sure terminal_render_loop can be sent across threads.
#[apply(test!)]
async fn test_terminal_render_loop_send() {
    let (term, _output) = Terminal::mock(MockTerminalConfig::default());
    await_send_future(terminal_render_loop(
        &mut element!(MyComponent),
        term,
        None,
        None,
    ))
    .await;
}

#[apply(test!)]
async fn test_terminal_render_loop_propagates_input_errors() {
    let term = Terminal::mock_with_event_error(io::Error::other("input failed"));

    let error = terminal_render_loop(&mut element!(View), term, None, None)
        .await
        .expect_err("terminal input errors should stop the render loop");

    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert_eq!(error.to_string(), "input failed");
}

#[component]
fn FullWidthComponent() -> impl Into<AnyElement<'static>> {
    element! {
        View(height: 2, width: 100pct, border_style: BorderStyle::Classic)
    }
}

#[test]
fn test_transparent_layout() {
    // For layout purposes, components defined with #[component] should not introduce a new
    // node in between its parent and child.
    let actual = element! {
        View(width: 10) {
            FullWidthComponent
        }
    }
    .to_string();
    assert_eq!(actual, "+--------+\n+--------+\n",);
}

#[derive(Default, Props)]
struct AsyncTickerProps {
    ticks: Option<State<i32>>,
}

#[component]
fn AsyncTicker<'a>(props: &mut AsyncTickerProps, mut hooks: Hooks) -> impl Into<AnyElement<'a>> {
    let mut ticks = props.ticks.unwrap();
    hooks.use_future(async move {
        ticks += 1;
    });
    element!(View)
}

#[component]
fn AsyncTickerContainer(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let child_ticks = hooks.use_state(|| 0);
    let mut tick = hooks.use_state(|| 0);

    hooks.use_future(async move {
        tick += 1;
    });

    if tick == 5 {
        // make sure our children have all ticked exactly 10 times
        assert_eq!(child_ticks, 10);
        system.exit();
    } else {
        // do a few more render passes
        tick += 1;
    }

    element! {
        View {
            #((0..10).map(|_| {
                element! {
                    AsyncTicker(ticks: child_ticks)
                }
            }))
        }
    }
}

// This is a regression test for an issue where elements added via iterator without keys would
// be re-created on every render instead of being recycled.
#[apply(test!)]
async fn test_async_ticker_container() {
    let canvases: Vec<_> = mock_terminal_render_loop(
        &mut element!(AsyncTickerContainer),
        MockTerminalConfig::default(),
    )
    .collect()
    .await;
    assert!(!canvases.is_empty());
}

#[test]
fn test_negative_dimensions() {
    let actual = element! {
        View(width: 10, height: 5, position: Position::Relative) {
            View(position: Position::Absolute, left: 10, top: 10, right: 10, bottom: 10, overflow: Overflow::Hidden) {
                Text(content: "Hello!")
            }
        }
    }
    .to_string();
    assert_eq!(actual, "\n\n\n\n\n",);
}

#[component]
fn NestedRows() -> impl Into<AnyElement<'static>> {
    element! {
        View(flex_direction: FlexDirection::Column) {
            #((0..3).map(|row| element! {
                View(key: row) {
                    Text(content: format!("row {row}"))
                }
            }))
        }
    }
}

// Shows and hides a nested subtree under a transparent wrapper twice, then exits.
#[component]
fn ToggledSubtreeApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let step = hooks.use_state(|| 0u8);
    let mut step_for_future = step;
    hooks.use_future(async move {
        for _ in 0..4 {
            smol::Timer::after(Duration::from_millis(20)).await;
            step_for_future.set(step_for_future.get() + 1);
        }
    });
    if step.get() == 4 {
        system.exit();
    }
    element! {
        View(flex_direction: FlexDirection::Column) {
            Text(content: format!("step {}", step.get()))
            #((step.get() % 2 == 1).then(|| element! {
                Fragment {
                    NestedRows
                }
            }))
        }
    }
}

#[apply(test!)]
async fn test_unmounted_subtrees_leave_no_layout_nodes() {
    let node_counts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let node_counts_cb = std::sync::Arc::clone(&node_counts);
    let _frames = element!(ToggledSubtreeApp)
        .mock_terminal_render_loop_with_profile(MockTerminalConfig::default(), move |event| {
            node_counts_cb
                .lock()
                .unwrap()
                .push(event.phases.layout_nodes)
        })
        .collect::<Vec<_>>()
        .await;
    let node_counts = node_counts.lock().unwrap().clone();
    let distinct: std::collections::BTreeSet<_> = node_counts.iter().collect();
    assert_eq!(
        distinct.len(),
        2,
        "every hidden frame has the same nodes, as does every shown one: {node_counts:?}"
    );
    assert_eq!(node_counts.first(), node_counts.last(), "{node_counts:?}");
}

// Puts a view with a text, a text, then a view with a text again under one explicit key.
#[component]
fn KeyedTypeSwapApp(mut hooks: Hooks) -> impl Into<AnyElement<'static>> {
    let mut system = hooks.use_context_mut::<SystemContext>();
    let step = hooks.use_state(|| 0u8);
    let mut step_for_future = step;
    hooks.use_future(async move {
        for _ in 0..2 {
            smol::Timer::after(Duration::from_millis(20)).await;
            step_for_future.set(step_for_future.get() + 1);
        }
    });
    if step.get() == 2 {
        system.exit();
    }
    let child: AnyElement<'static> = if step.get() == 1 {
        let mut text = element!(Text(content: "text"));
        text.key = ElementKey::new("same");
        text.into()
    } else {
        let mut view = element! {
            View {
                Text(content: "view")
            }
        };
        view.key = ElementKey::new("same");
        view.into()
    };
    element! {
        View {
            #(child)
        }
    }
}

#[apply(test!)]
async fn test_a_keyed_child_replaced_by_another_type_leaves_no_layout_nodes() {
    let node_counts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let node_counts_cb = std::sync::Arc::clone(&node_counts);
    let _frames = element!(KeyedTypeSwapApp)
        .mock_terminal_render_loop_with_profile(MockTerminalConfig::default(), move |event| {
            node_counts_cb
                .lock()
                .unwrap()
                .push(event.phases.layout_nodes)
        })
        .collect::<Vec<_>>()
        .await;
    let node_counts = node_counts.lock().unwrap().clone();
    assert_eq!(node_counts.first(), node_counts.last(), "{node_counts:?}");
}
