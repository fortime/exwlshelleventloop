use std::{collections::HashMap, mem};

use iced::{
    Event, Size,
    event::Status,
    mouse::{Cursor, Interaction},
    window::Id,
};
use iced_core::{Clipboard, renderer::Style, widget::Operation};
use iced_runtime::{
    Debug as IcedDebug, Program as SingleWindowProgram, UserInterface as IcedUserInterface,
    multi_window::Program as MultiWindowProgram,
    user_interface::{Cache, State},
};

pub(crate) trait UserInterfaceReclaim<Message, Theme, Renderer> {
    fn reclaim(&mut self, ui: IcedUserInterface<'static, Message, Theme, Renderer>);
}

/// Provide a guard to hold the ui and prevent leaking the reference to the application. A user can hold this guard without querying from map each time.
/// When this guard is dropped, it will return the ui to the manager if it is not taken.
pub(crate) struct UserInterfaceMutGuard<'a, Message, Theme, Renderer, Reclaim>
where
    Reclaim: UserInterfaceReclaim<Message, Theme, Renderer>,
{
    reclaim: Reclaim,
    /// Building 'static IcedUserInterface will draw nothing, so we should use the safe lifetime as
    /// application.
    ui: Option<IcedUserInterface<'a, Message, Theme, Renderer>>,
}

impl<'a, Message, Theme, Renderer, Reclaim>
    UserInterfaceMutGuard<'a, Message, Theme, Renderer, Reclaim>
where
    Renderer: iced_core::Renderer,
    Reclaim: UserInterfaceReclaim<Message, Theme, Renderer>,
{
    fn take(&mut self) -> IcedUserInterface<'a, Message, Theme, Renderer> {
        self.ui.take().expect("ui is taken")
    }

    pub fn draw(
        &mut self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &Style,
        cursor: Cursor,
    ) -> Interaction {
        let mut ui = self.take();
        let interaction = ui.draw(renderer, theme, style, cursor);
        self.ui = Some(ui);
        interaction
    }

    #[allow(unused)]
    pub fn into_cache(mut self) -> Cache {
        self.take().into_cache()
    }

    pub fn operate(&mut self, renderer: &Renderer, operation: &mut dyn Operation<()>) {
        let mut ui = self.take();
        ui.operate(renderer, operation);
        self.ui = Some(ui);
    }

    pub fn relayout(mut self, bounds: Size, renderer: &mut Renderer) -> Self {
        let ui = self.take().relayout(bounds, renderer);
        self.ui = Some(ui);
        self
    }

    pub fn update(
        &mut self,
        events: &[Event],
        cursor: Cursor,
        renderer: &mut Renderer,
        clipboard: &mut dyn Clipboard,
        messages: &mut Vec<Message>,
    ) -> (State, Vec<Status>) {
        let mut ui = self.take();
        let res = ui.update(events, cursor, renderer, clipboard, messages);
        self.ui = Some(ui);
        res
    }
}

impl<Message, Theme, Renderer, Reclaim> Drop
    for UserInterfaceMutGuard<'_, Message, Theme, Renderer, Reclaim>
where
    Reclaim: UserInterfaceReclaim<Message, Theme, Renderer>,
{
    fn drop(&mut self) {
        if let Some(ui) = self.ui.take() {
            // SAFETY There is no public api to change ui. It always refers to application.
            let ui: IcedUserInterface<'static, _, _, _> = unsafe { mem::transmute(ui) };
            self.reclaim.reclaim(ui);
        }
    }
}

pub struct UserInterfaces<A, Message, Theme, Renderer> {
    // SAFETY application will only be dropped after all uis are dropped. And we won't
    // allow publicly access to IcedUserInterface<'static, A::Message, A::Theme, A::Renderer>, so
    // reference to application won't be leaked to public.
    uis: HashMap<Id, IcedUserInterface<'static, Message, Theme, Renderer>>,
    application: Box<A>,
}

impl<A> UserInterfaces<A, A::Message, A::Theme, A::Renderer>
where
    A: MultiWindowProgram + 'static,
{
    pub fn new(application: A) -> Self {
        Self {
            uis: HashMap::new(),
            application: Box::new(application),
        }
    }

    pub fn application(&self) -> &A {
        &self.application
    }

    pub fn remove(&mut self, id: &Id) -> Option<Cache> {
        self.uis.remove(id).map(IcedUserInterface::into_cache)
    }

    pub fn extract_all(&mut self) -> (HashMap<Id, Cache>, &mut A) {
        // SAFETY remove all references before return mut reference of application
        let caches = self
            .uis
            .drain()
            .map(|(id, ui)| (id, ui.into_cache()))
            .collect();
        (caches, &mut self.application)
    }

    #[allow(clippy::type_complexity)]
    pub fn ui_mut(
        &mut self,
        id: &Id,
    ) -> Option<UserInterfaceMutGuard<'static, A::Message, A::Theme, A::Renderer, (&mut Self, Id)>>
    {
        self.uis.remove(id).map(|ui| UserInterfaceMutGuard {
            reclaim: (self, *id),
            ui: Some(ui),
        })
    }

    pub fn build(
        &mut self,
        id: Id,
        cache: Cache,
        renderer: &mut A::Renderer,
        size: Size,
        debug: &mut IcedDebug,
    ) {
        debug.view_started();
        let view = self.application.view(id);
        debug.view_finished();

        debug.layout_started();
        let ui = IcedUserInterface::build(view, size, cache, renderer);
        debug.layout_finished();
        // SAFETY ui won't outlive application.
        let ui: IcedUserInterface<'static, _, _, _> = unsafe { mem::transmute(ui) };
        self.uis.insert(id, ui);
    }
}

impl<A, Message, Theme, Renderer> Drop for UserInterfaces<A, Message, Theme, Renderer> {
    fn drop(&mut self) {
        // SAFETY drop all references of application before dropping application
        self.uis.clear();
    }
}

impl<A, Message, Theme, Renderer> UserInterfaceReclaim<Message, Theme, Renderer>
    for (&mut UserInterfaces<A, Message, Theme, Renderer>, Id)
{
    fn reclaim(&mut self, ui: IcedUserInterface<'static, Message, Theme, Renderer>) {
        self.0.uis.insert(self.1, ui);
    }
}

#[allow(unused)]
pub struct UserInterface<A, Message, Theme, Renderer> {
    // SAFETY application will only be dropped after ui is dropped. And we won't allow
    // publicly access to IcedUserInterface<'static, A::Message, A::Theme, A::Renderer>, so
    // reference to application won't be leaked to public.
    ui: Option<IcedUserInterface<'static, Message, Theme, Renderer>>,
    application: Box<A>,
}

#[allow(unused)]
impl<A> UserInterface<A, A::Message, A::Theme, A::Renderer>
where
    A: SingleWindowProgram + 'static,
{
    pub fn new(application: A) -> Self {
        Self {
            ui: None,
            application: Box::new(application),
        }
    }

    pub fn is_built(&self) -> bool {
        self.ui.is_some()
    }

    pub fn application(&self) -> &A {
        &self.application
    }

    pub fn extract(&mut self) -> (Option<Cache>, &mut A) {
        // SAFETY remove all references before return mut reference of application
        let cache = self.ui.take().map(|ui| ui.into_cache());
        (cache, &mut self.application)
    }

    #[allow(clippy::type_complexity)]
    pub fn ui_mut(
        &mut self,
    ) -> Option<UserInterfaceMutGuard<'_, A::Message, A::Theme, A::Renderer, &mut Self>> {
        self.ui.take().map(|ui| UserInterfaceMutGuard {
            reclaim: self,
            ui: Some(ui),
        })
    }

    pub fn build(
        &mut self,
        cache: Cache,
        renderer: &mut A::Renderer,
        size: Size,
        debug: &mut IcedDebug,
    ) {
        debug.view_started();
        let view = self.application.view();
        debug.view_finished();

        debug.layout_started();
        let ui = IcedUserInterface::build(view, size, cache, renderer);
        debug.layout_finished();
        // SAFETY ui won't outlive application.
        let ui: IcedUserInterface<'static, _, _, _> = unsafe { mem::transmute(ui) };
        self.ui = Some(ui);
    }
}

impl<A, Message, Theme, Renderer> Drop for UserInterface<A, Message, Theme, Renderer> {
    fn drop(&mut self) {
        // SAFETY drop all references of application before dropping application
        drop(self.ui.take());
    }
}

impl<A, Message, Theme, Renderer> UserInterfaceReclaim<Message, Theme, Renderer>
    for &mut UserInterface<A, Message, Theme, Renderer>
{
    fn reclaim(&mut self, ui: IcedUserInterface<'static, Message, Theme, Renderer>) {
        self.ui = Some(ui);
    }
}
