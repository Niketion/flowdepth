use iced::{
    Element, Event, Length, Rectangle, Size, Vector,
    advanced::{Layout, Shell, Widget, layout, mouse, overlay, renderer, widget::Tree},
};

/// A small drag surface that captures the pointer until it is released.
/// Unlike `mouse_area`, dragging continues after the pointer leaves the handle.
pub struct DragHandle<'a, Message, Theme, Renderer> {
    content: Element<'a, Message, Theme, Renderer>,
    on_start: Box<dyn Fn(iced::Point) -> Message + 'a>,
    on_drag: Box<dyn Fn(iced::Point) -> Message + 'a>,
    on_end: Message,
}

#[derive(Default)]
struct State {
    dragging: bool,
}

pub fn drag_handle<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    on_start: impl Fn(iced::Point) -> Message + 'a,
    on_drag: impl Fn(iced::Point) -> Message + 'a,
    on_end: Message,
) -> DragHandle<'a, Message, Theme, Renderer> {
    DragHandle {
        content: content.into(),
        on_start: Box::new(on_start),
        on_drag: Box::new(on_drag),
        on_end,
    }
}

impl<Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for DragHandle<'_, Message, Theme, Renderer>
where
    Message: Clone,
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> iced::advanced::widget::tree::Tag {
        iced::advanced::widget::tree::Tag::of::<State>()
    }
    fn state(&self) -> iced::advanced::widget::tree::State {
        iced::advanced::widget::tree::State::new(State::default())
    }
    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let state: &mut State = tree.state.downcast_mut();
        if state.dragging {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    if let Some(p) = cursor.position() {
                        shell.publish((self.on_drag)(p));
                        shell.capture_event();
                    }
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    state.dragging = false;
                    shell.publish(self.on_end.clone());
                    shell.capture_event();
                }
                _ => {}
            }
            return;
        }
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            shell,
            viewport,
        );
        if !shell.is_event_captured()
            && cursor.is_over(layout.bounds())
            && matches!(
                event,
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            )
        {
            state.dragging = true;
            if let Some(p) = cursor.position() {
                shell.publish((self.on_start)(p));
            }
            shell.capture_event();
        }
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if tree.state.downcast_ref::<State>().dragging || cursor.is_over(layout.bounds()) {
            mouse::Interaction::Grabbing
        } else {
            self.content.as_widget().mouse_interaction(
                &tree.children[0],
                layout,
                cursor,
                viewport,
                renderer,
            )
        }
    }
    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
    }
    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'b, Message, Theme, Renderer>> {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<DragHandle<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a + Clone,
    Theme: 'a,
    Renderer: 'a + renderer::Renderer,
{
    fn from(value: DragHandle<'a, Message, Theme, Renderer>) -> Self {
        Element::new(value)
    }
}
