mod state;

use std::{
    borrow::Cow, collections::VecDeque, mem, os::fd::AsFd, sync::Arc, task::Poll, time::Duration,
};

use crate::{
    LayershellCustomActions,
    clipboard::LayerShellClipboard,
    conversion,
    error::Error,
    event::{WaitingLayerShellEvent, WindowEvent as LayerWindowEvent},
    settings::VirtualKeyboardSettings,
    user_interface::UserInterface,
};

use super::{Appearance, DefaultStyle};
use iced::mouse::Interaction;
use iced_graphics::{Compositor, compositor};
use state::State;

use iced_core::{
    Event as IcedEvent,
    time::Instant,
    window::{Event as IcedWindowEvent, Id as IcedId},
};

use iced_runtime::{Action, Debug, Program, task::Task, user_interface};

use iced_futures::{Executor, Runtime, Subscription};

use layershellev::{
    LayerEvent, ReturnData, StartMode, WindowStateSimple, WindowWrapper,
    calloop::timer::{TimeoutAction, Timer},
    reexport::{
        wayland_client::{WlCompositor, WlRegion},
        zwp_virtual_keyboard_v1,
    },
};

use futures::{FutureExt, future::LocalBoxFuture};

use crate::{proxy::IcedProxy, settings::Settings};

/// An interactive, native cross-platform application.
///
/// This trait is the main entrypoint of Iced. Once implemented, you can run
/// your GUI application by simply calling [`run`]. It will run in
/// its own window.
///
/// An [`Application`] can execute asynchronous actions by returning a
/// [`Task`] in some of its methods.
///
/// When using an [`Application`] with the `debug` feature enabled, a debug view
/// can be toggled by pressing `F12`.
pub trait Application: Program
where
    Self::Theme: DefaultStyle,
{
    /// The data needed to initialize your [`Application`].
    type Flags;

    /// Initializes the [`Application`] with the flags provided to
    /// [`run`] as part of the [`Settings`].
    ///
    /// Here is where you should return the initial state of your app.
    ///
    /// Additionally, you can return a [`Task`] if you need to perform some
    /// async action in the background on startup. This is useful if you want to
    /// load state from a file, perform an initial HTTP request, etc.
    fn new(flags: Self::Flags) -> (Self, Task<Self::Message>);

    fn namespace(&self) -> String;
    /// Returns the current title of the [`Application`].
    ///
    /// This title can be dynamic! The runtime will automatically update the
    /// title of your application when necessary.
    fn title(&self) -> String {
        self.namespace()
    }

    /// Returns the current `Theme` of the [`Application`].
    fn theme(&self) -> Self::Theme;

    /// Returns the `Style` variation of the `Theme`.
    fn style(&self, theme: &Self::Theme) -> Appearance {
        theme.default_style()
    }

    /// Returns the event `Subscription` for the current state of the
    /// application.
    ///
    /// The messages produced by the `Subscription` will be handled by
    /// [`update`](#tymethod.update).
    ///
    /// A `Subscription` will be kept alive as long as you keep returning it!
    ///
    /// By default, it returns an empty subscription.
    fn subscription(&self) -> Subscription<Self::Message> {
        Subscription::none()
    }

    /// Returns the scale factor of the [`Application`].
    ///
    /// It can be used to dynamically control the size of the UI at runtime
    /// (i.e. zooming).
    ///
    /// For instance, a scale factor of `2.0` will make widgets twice as big,
    /// while a scale factor of `0.5` will shrink them to half their size.
    ///
    /// By default, it returns `1.0`.
    fn scale_factor(&self) -> f64 {
        1.0
    }

    /// Defines whether or not to use natural scrolling
    fn natural_scroll(&self) -> bool {
        false
    }

    /// Returns whether the [`Application`] should be terminated.
    ///
    /// By default, it returns `false`.
    fn should_exit(&self) -> bool {
        false
    }
}

type SingleRuntime<E, Message> = Runtime<E, IcedProxy<Action<Message>>, Action<Message>>;

// a dispatch loop, another is listen loop
pub fn run<A, E, C>(
    settings: Settings<A::Flags>,
    compositor_settings: iced_graphics::Settings,
) -> Result<(), Error>
where
    A: Application + 'static,
    E: Executor + 'static,
    C: Compositor<Renderer = A::Renderer> + 'static,
    A::Theme: DefaultStyle,
    A::Message: 'static + TryInto<LayershellCustomActions, Error = A::Message>,
{
    use futures::task;

    let mut debug = Debug::new();
    debug.startup_started();

    let (message_sender, message_receiver) = std::sync::mpsc::channel::<Action<A::Message>>();

    let proxy = IcedProxy::new(message_sender);
    let mut runtime: SingleRuntime<E, A::Message> = {
        let executor = E::new().map_err(Error::ExecutorCreationFailed)?;

        Runtime::new(executor, proxy)
    };

    let (application, task) = {
        let flags = settings.flags;

        runtime.enter(|| A::new(flags))
    };

    assert!(!matches!(
        settings.layer_settings.start_mode,
        StartMode::AllScreens | StartMode::Background
    ));

    let ev = layershellev::WindowStateSimple::new(&application.namespace())
        .with_use_display_handle(true)
        .with_option_size(settings.layer_settings.size)
        .with_layer(settings.layer_settings.layer)
        .with_events_transparent(settings.layer_settings.events_transparent)
        .with_anchor(settings.layer_settings.anchor)
        .with_exclusive_zone(settings.layer_settings.exclusive_zone)
        .with_margin(settings.layer_settings.margin)
        .with_keyboard_interacivity(settings.layer_settings.keyboard_interactivity)
        .with_start_mode(settings.layer_settings.start_mode)
        .build()
        .expect("Cannot create layershell");

    let window = Arc::new(ev.gen_mainwindow_wrapper());

    if let Some(stream) = iced_runtime::task::into_stream(task) {
        runtime.run(stream);
    }

    runtime.track(iced_futures::subscription::into_recipes(
        runtime.enter(|| application.subscription().map(Action::Output)),
    ));

    let state = State::new(&application, &ev);

    let context = Context::<A, E, C>::new(
        application,
        compositor_settings,
        runtime,
        state,
        window,
        debug,
        settings.fonts,
    );
    let mut context_state = ContextState::Context(context);

    let mut waiting_layer_shell_events = VecDeque::new();
    let mut task_context = task::Context::from_waker(task::noop_waker_ref());

    let _ = ev.running_with_proxy(message_receiver, move |event, ev, _| {
        let mut def_returndata = ReturnData::None;
        match event {
            LayerEvent::InitRequest => {
                def_returndata = ReturnData::RequestBind;
            }
            LayerEvent::BindProvide(globals, qh) => {
                let wl_compositor = globals
                    .bind::<WlCompositor, _, _>(qh, 1..=1, ())
                    .expect("could not bind wl_compositor");
                waiting_layer_shell_events.push_back(WaitingLayerShellEvent::UpdateInputRegion(
                    wl_compositor.create_region(qh, ()),
                ));

                if settings.virtual_keyboard_support.is_some() {
                    let virtual_keyboard_manager = globals
                        .bind::<zwp_virtual_keyboard_v1::ZwpVirtualKeyboardManagerV1, _, _>(
                            qh,
                            1..=1,
                            (),
                        )
                        .expect("no support virtual_keyboard");
                    let VirtualKeyboardSettings {
                        file,
                        keymap_size,
                        keymap_format,
                    } = settings.virtual_keyboard_support.as_ref().unwrap();
                    let seat = ev.get_seat();
                    let virtual_keyboard_in =
                        virtual_keyboard_manager.create_virtual_keyboard(seat, qh, ());
                    virtual_keyboard_in.keymap((*keymap_format).into(), file.as_fd(), *keymap_size);
                    ev.set_virtual_keyboard(virtual_keyboard_in);
                }
            }
            LayerEvent::RequestMessages(message) => {
                waiting_layer_shell_events.push_back(WaitingLayerShellEvent::Window(
                    LayerWindowEvent::from(message),
                ));
            }
            LayerEvent::UserEvent(event) => {
                waiting_layer_shell_events.push_back(WaitingLayerShellEvent::UserAction(event));
            }
            LayerEvent::NormalDispatch => {
                waiting_layer_shell_events.push_back(WaitingLayerShellEvent::NormalDispatch);
            }
            _ => {}
        }
        loop {
            let mut need_continue = false;
            context_state = match std::mem::replace(&mut context_state, ContextState::None) {
                ContextState::None => unreachable!("context state is taken but not returned"),
                ContextState::Future(mut future) => {
                    tracing::debug!("poll context future");
                    match future.as_mut().poll(&mut task_context) {
                        Poll::Ready(context) => {
                            tracing::debug!("context future is ready");
                            // context is ready, continue to run.
                            need_continue = true;
                            ContextState::Context(context)
                        }
                        Poll::Pending => ContextState::Future(future),
                    }
                }
                ContextState::Context(context) => {
                    if let Some(layer_shell_event) = waiting_layer_shell_events.pop_front() {
                        need_continue = true;
                        let (context_state, waiting_layer_shell_event) =
                            context.handle_event(ev, layer_shell_event);
                        if let Some(waiting_layer_shell_event) = waiting_layer_shell_event {
                            waiting_layer_shell_events.push_front(waiting_layer_shell_event);
                        }
                        context_state
                    } else {
                        ContextState::Context(context)
                    }
                }
            };
            if !need_continue {
                break;
            }
        }
        def_returndata
    });
    Ok(())
}

enum ContextState<Context> {
    None,
    Context(Context),
    Future(LocalBoxFuture<'static, Context>),
}

struct Context<A, E, C>
where
    A: Application + 'static,
    C: Compositor<Renderer = A::Renderer> + 'static,
    E: Executor + 'static,
    A::Theme: DefaultStyle,
    A::Message: 'static,
{
    iced_id: IcedId,
    compositor_settings: iced_graphics::Settings,
    runtime: SingleRuntime<E, A::Message>,
    state: State<A>,
    layer_shell_window_wrapper: Arc<WindowWrapper>,
    mouse_interaction: Interaction,
    debug: Debug,
    fonts: Vec<Cow<'static, [u8]>>,
    compositor_context: Option<(C, C::Surface, A::Renderer)>,
    clipboard: LayerShellClipboard,
    wl_input_region: Option<WlRegion>,
    user_interface: UserInterface<A, A::Message, A::Theme, A::Renderer>,
    waiting_layer_shell_actions: Vec<LayershellCustomActions>,
    iced_events: Vec<IcedEvent>,
    messages: Vec<A::Message>,
}

impl<A, E, C> Context<A, E, C>
where
    A: Application + 'static,
    C: Compositor<Renderer = A::Renderer> + 'static,
    E: Executor + 'static,
    A::Theme: DefaultStyle,
    A::Message: 'static + TryInto<LayershellCustomActions, Error = A::Message>,
{
    pub fn new(
        application: A,
        compositor_settings: iced_graphics::Settings,
        runtime: SingleRuntime<E, A::Message>,
        state: State<A>,
        layer_shell_window_wrapper: Arc<WindowWrapper>,
        debug: Debug,
        fonts: Vec<Cow<'static, [u8]>>,
    ) -> Self {
        Self {
            iced_id: IcedId::unique(),
            compositor_settings,
            runtime,
            state,
            layer_shell_window_wrapper,
            mouse_interaction: Default::default(),
            debug,
            fonts,
            compositor_context: Default::default(),
            clipboard: LayerShellClipboard::unconnected(),
            wl_input_region: Default::default(),
            user_interface: UserInterface::new(application),
            waiting_layer_shell_actions: Default::default(),
            iced_events: Default::default(),
            messages: Default::default(),
        }
    }

    async fn create_compositor_context(mut self) -> Self {
        let mut new_compositor = C::new(
            self.compositor_settings,
            self.layer_shell_window_wrapper.clone(),
        )
        .await
        .expect("Cannot create compositer");
        for font in self.fonts.clone() {
            new_compositor.load_font(font);
        }

        let renderer = new_compositor.create_renderer();
        // HACK: the surface size should not be set as 0, 0
        // but it will changed later
        // so here set it to 1, 1
        let surface = new_compositor.create_surface(self.layer_shell_window_wrapper.clone(), 1, 1);
        self.compositor_context = Some((new_compositor, surface, renderer));

        self.clipboard = LayerShellClipboard::connect(&self.layer_shell_window_wrapper);
        self
    }

    fn handle_event(
        mut self,
        ev: &mut WindowStateSimple,
        layer_shell_event: WaitingLayerShellEvent<A::Message>,
    ) -> (
        ContextState<Self>,
        Option<WaitingLayerShellEvent<A::Message>>,
    ) {
        tracing::debug!(
            "Handle layer shell event, event: {:?}, waiting actions: {}, messages: {}",
            layer_shell_event,
            self.waiting_layer_shell_actions.len(),
            self.messages.len(),
        );
        if self.compositor_context.is_none() {
            tracing::debug!("creating compositor");
            let context_state =
                ContextState::Future(self.create_compositor_context().boxed_local());
            return (context_state, Some(layer_shell_event));
        }

        match layer_shell_event {
            WaitingLayerShellEvent::UpdateInputRegion(region) => {
                self.wl_input_region = Some(region)
            }
            WaitingLayerShellEvent::Window(LayerWindowEvent::Refresh) => {
                self.handle_refresh_event(ev)
            }
            WaitingLayerShellEvent::Window(LayerWindowEvent::Closed) => {
                ev.append_return_data(ReturnData::RequestExit)
            }
            WaitingLayerShellEvent::Window(window_event) => self.handle_window_event(window_event),
            WaitingLayerShellEvent::UserAction(user_action) => {
                self.handle_user_action(ev, user_action)
            }
            WaitingLayerShellEvent::NormalDispatch => self.handle_normal_dispatch(ev),
        }

        // at each interaction try to resolve those waiting actions.
        let mut waiting_layer_shell_actions = Vec::new();
        mem::swap(
            &mut self.waiting_layer_shell_actions,
            &mut waiting_layer_shell_actions,
        );
        for action in waiting_layer_shell_actions {
            self.handle_layer_shell_action(ev, action);
        }

        (ContextState::Context(self), None)
    }

    fn handle_refresh_event(&mut self, ev: &mut WindowStateSimple) {
        let (compositor, surface, renderer) = self
            .compositor_context
            .as_mut()
            .expect("compositor context is not initialized");
        let layer_shell_window = ev.main_window();
        let (width, height) = layer_shell_window.get_size();
        let scale_float = layer_shell_window.scale_float();
        // events may not be handled after RequestRefreshWithWrapper in the same
        // interaction, we dispatched them immediately.
        let mut events = Vec::new();

        if !self.user_interface.is_built() {
            self.state.update_view_port(width, height, scale_float);

            self.user_interface.build(
                user_interface::Cache::default(),
                renderer,
                self.state.viewport().logical_size(),
                &mut self.debug,
            );

            // update the size of the suface after created.
            let physical_size = self.state.viewport().physical_size();
            compositor.configure_surface(surface, physical_size.width, physical_size.height);

            events.push(IcedEvent::Window(IcedWindowEvent::Opened {
                position: None,
                size: self.state.window_size_f32(),
            }));
        };

        let mut ui = self.user_interface.ui_mut().expect("ui not built");

        let window_size = self.state.window_size();

        if window_size.width != width
            || window_size.height != height
            || self.state.wayland_scale_factor() != scale_float
        {
            self.state.update_view_port(width, height, scale_float);
            ui = ui.relayout(self.state.viewport().logical_size(), renderer);

            let physical_size = self.state.viewport().physical_size();
            compositor.configure_surface(surface, physical_size.width, physical_size.height);
        }

        let cursor = self.state.cursor();

        events.push(IcedEvent::Window(IcedWindowEvent::RedrawRequested(
            Instant::now(),
        )));
        let (_, statuses) = ui.update(
            &events,
            cursor,
            renderer,
            &mut self.clipboard,
            &mut self.messages,
        );

        for (idx, event) in events.into_iter().enumerate() {
            let status = statuses
                .get(idx)
                .cloned()
                .unwrap_or(iced_core::event::Status::Ignored);
            self.runtime
                .broadcast(iced_futures::subscription::Event::Interaction {
                    window: self.iced_id,
                    event,
                    status,
                });
        }
        self.debug.render_started();

        self.debug.draw_started();
        let new_mouse_interaction = ui.draw(
            renderer,
            self.state.theme(),
            &iced_core::renderer::Style {
                text_color: self.state.text_color(),
            },
            cursor,
        );
        self.debug.draw_finished();

        if new_mouse_interaction != self.mouse_interaction {
            if let Some(pointer) = ev.get_pointer() {
                ev.append_return_data(ReturnData::RequestSetCursorShape((
                    conversion::mouse_interaction(new_mouse_interaction),
                    pointer.clone(),
                )));
            }
            self.mouse_interaction = new_mouse_interaction;
        }

        match compositor.present(
            renderer,
            surface,
            self.state.viewport(),
            self.state.background_color(),
            &self.debug.overlay(),
        ) {
            Ok(()) => {
                self.debug.render_finished();
            }
            Err(error) => match error {
                compositor::SurfaceError::OutOfMemory => {
                    panic!("{:?}", error);
                }
                _ => {
                    // we can't reset the present available state here, the window will
                    // will never be redrawn.
                    panic!("Error {error:?} when presenting surface.");
                }
            },
        }
    }

    fn handle_window_event(&mut self, event: LayerWindowEvent) {
        self.state.update(&event);
        if let Some(event) = conversion::window_event(
            &event,
            self.state.application_scale_factor(),
            self.state.modifiers(),
        ) {
            self.iced_events.push(event);
        }
    }

    fn handle_user_action(&mut self, ev: &mut WindowStateSimple, action: Action<A::Message>) {
        let (compositor, surface, renderer) = self
            .compositor_context
            .as_mut()
            .expect("compositor context is not initialized");
        let mut should_exit = false;
        run_action(
            &mut self.user_interface,
            compositor,
            surface,
            &self.state,
            renderer,
            action,
            &mut self.messages,
            &mut self.clipboard,
            &mut self.waiting_layer_shell_actions,
            &mut should_exit,
            &mut self.debug,
        );
        if should_exit {
            ev.append_return_data(ReturnData::RequestExit);
        }
    }

    fn handle_layer_shell_action(
        &mut self,
        ev: &mut WindowStateSimple,
        action: LayershellCustomActions,
    ) {
        match action {
            LayershellCustomActions::AnchorChange(anchor) => {
                ev.main_window().set_anchor(anchor);
            }
            LayershellCustomActions::AnchorSizeChange(anchor, size) => {
                ev.main_window().set_anchor_with_size(anchor, size);
            }
            LayershellCustomActions::LayerChange(layer) => {
                ev.main_window().set_layer(layer);
            }
            LayershellCustomActions::MarginChange(margin) => {
                ev.main_window().set_margin(margin);
            }
            LayershellCustomActions::SizeChange((width, height)) => {
                ev.main_window().set_size((width, height));
            }
            LayershellCustomActions::ExclusiveZoneChange(zone_size) => {
                ev.main_window().set_exclusive_zone(zone_size);
            }
            LayershellCustomActions::SetInputRegion(set_region) => {
                let layer_shell_window = ev.main_window();
                let set_region = set_region.0;
                let Some(region) = &self.wl_input_region else {
                    tracing::warn!("wl_input_region is not set, ignore SetInputRegion",);
                    return;
                };

                let window_size = layer_shell_window.get_size();
                let width: i32 = window_size.0.try_into().unwrap_or_default();
                let height: i32 = window_size.1.try_into().unwrap_or_default();

                region.subtract(0, 0, width, height);
                set_region(region);

                layer_shell_window
                    .get_wlsurface()
                    .set_input_region(self.wl_input_region.as_ref());
            }
            LayershellCustomActions::VirtualKeyboardPressed { time, key } => {
                use layershellev::reexport::wayland_client::KeyState;
                let ky = ev.get_virtual_keyboard().unwrap();
                ky.key(time, key, KeyState::Pressed.into());

                let eh = ev.get_loop_handler().unwrap();
                eh.insert_source(
                    Timer::from_duration(Duration::from_micros(100)),
                    move |_, _, state| {
                        let ky = state.get_virtual_keyboard().unwrap();

                        ky.key(time, key, KeyState::Released.into());
                        TimeoutAction::Drop
                    },
                )
                .ok();
            }
            _ => {}
        }
    }

    fn handle_normal_dispatch(&mut self, ev: &mut WindowStateSimple) {
        if self.iced_events.is_empty() && self.messages.is_empty() {
            return;
        }

        let (_, _, renderer) = self
            .compositor_context
            .as_mut()
            .expect("compositor context is not initialized");

        self.debug.event_processing_started();

        let (ui_state, statuses) = if let Some(mut ui) = self.user_interface.ui_mut() {
            ui.update(
                &self.iced_events,
                self.state.cursor(),
                renderer,
                &mut self.clipboard,
                &mut self.messages,
            )
        } else {
            // ui hasn't been built skip
            ev.request_refresh_all();
            return;
        };

        let mut rebuilt = false;
        match ui_state {
            user_interface::State::Outdated => rebuilt = true,
            // TODO support redraw at
            user_interface::State::Updated {
                redraw_request: Some(_),
            } => {}
            user_interface::State::Updated {
                redraw_request: None,
            } => {
                // no redraw
                // custom_actions.pop();

                // redraw anyway. in iced 0.13.1, most widget doesn't set redraw
                // request.
            }
        }

        self.debug.event_processing_finished();

        for (event, status) in self.iced_events.drain(..).zip(statuses.into_iter()) {
            self.runtime
                .broadcast(iced_futures::subscription::Event::Interaction {
                    window: self.iced_id,
                    event,
                    status,
                });
        }

        if !self.messages.is_empty() {
            let (cache, application) = self.user_interface.extract();

            // Update application
            update(
                application,
                &mut self.state,
                &mut self.runtime,
                &mut self.debug,
                &mut self.messages,
            );

            self.user_interface.build(
                cache.unwrap_or_default(),
                renderer,
                self.state.viewport().logical_size(),
                &mut self.debug,
            );
        } else if rebuilt {
            let (cache, _) = self.user_interface.extract();
            self.user_interface.build(
                cache.unwrap_or_default(),
                renderer,
                self.state.viewport().logical_size(),
                &mut self.debug,
            );
        }
        ev.request_refresh_all();
    }
}

/// Updates an [`Application`] by feeding it the provided messages, spawning any
/// tracking its [`Subscription`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn update<A: Application, E: Executor>(
    application: &mut A,
    state: &mut State<A>,
    runtime: &mut SingleRuntime<E, A::Message>,
    debug: &mut Debug,
    messages: &mut Vec<A::Message>,
) where
    A::Theme: DefaultStyle,
    A::Message: 'static,
{
    for message in messages.drain(..) {
        debug.log_message(&message);

        debug.update_started();
        let task = runtime.enter(|| application.update(message));
        debug.update_finished();

        if let Some(stream) = iced_runtime::task::into_stream(task) {
            runtime.run(stream);
        }
    }
    state.synchronize(application);

    let subscription = runtime.enter(|| application.subscription());
    runtime.track(iced_futures::subscription::into_recipes(
        subscription.map(Action::Output),
    ));
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_action<A, C>(
    user_interface: &mut UserInterface<A, A::Message, A::Theme, A::Renderer>,
    compositor: &mut C,
    surface: &mut C::Surface,
    state: &State<A>,
    renderer: &mut A::Renderer,
    event: Action<A::Message>,
    messages: &mut Vec<A::Message>,
    clipboard: &mut LayerShellClipboard,
    waiting_layer_shell_actions: &mut Vec<LayershellCustomActions>,
    should_exit: &mut bool,
    debug: &mut Debug,
) where
    A: Application + 'static,
    C: Compositor<Renderer = A::Renderer> + 'static,
    A::Theme: DefaultStyle,
    A::Message: 'static + TryInto<LayershellCustomActions, Error = A::Message>,
{
    use iced_core::widget::operation;
    use iced_runtime::Action;
    use iced_runtime::clipboard;
    use iced_runtime::window;
    use iced_runtime::window::Action as WindowAction;
    match event {
        Action::Output(stream) => match stream.try_into() {
            Ok(action) => waiting_layer_shell_actions.push(action),
            Err(stream) => {
                messages.push(stream);
            }
        },

        Action::Clipboard(action) => match action {
            clipboard::Action::Read { target, channel } => {
                let _ = channel.send(clipboard.read(target));
            }
            clipboard::Action::Write { target, contents } => {
                clipboard.write(target, contents);
            }
        },
        Action::Widget(action) => {
            let mut current_operation = Some(action);

            while let Some(mut operation) = current_operation.take() {
                if let Some(mut ui) = user_interface.ui_mut() {
                    ui.operate(renderer, operation.as_mut());
                }

                match operation.finish() {
                    operation::Outcome::None => {}
                    operation::Outcome::Some(_message) => {
                        // TODO:
                    }
                    operation::Outcome::Chain(next) => {
                        current_operation = Some(next);
                    }
                }
            }
        }
        Action::Window(action) => match action {
            WindowAction::Close(_) => {
                *should_exit = true;
            }
            WindowAction::Screenshot(_id, channel) => {
                let bytes = compositor.screenshot(
                    renderer,
                    surface,
                    state.viewport(),
                    state.background_color(),
                    &debug.overlay(),
                );
                let _ = channel.send(window::Screenshot::new(
                    bytes,
                    state.viewport().physical_size(),
                    state.viewport().scale_factor(),
                ));
            }
            WindowAction::GetScaleFactor(_id, channel) => {
                let _ = channel.send(state.wayland_scale_factor() as f32);
            }
            _ => {}
        },
        Action::Exit => {
            *should_exit = true;
        }
        Action::LoadFont { bytes, channel } => {
            // TODO: Error handling (?)
            compositor.load_font(bytes.clone());

            let _ = channel.send(Ok(()));
        }
        _ => {}
    }
}
