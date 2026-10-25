use gpui_kit::component::button::*;
use gpui_kit::component::*;
use gpui_kit::*;

pub struct Spike;

impl Render for Spike {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .v_flex()
            .gap_2()
            .size_full()
            .items_center()
            .justify_center()
            .child("hfr-gui spike")
            .child(Button::new("ok").primary().label("It works").on_click(|_, _, _| println!("clicked")))
    }
}

fn main() {
    gpui_kit::application().with_assets(gpui_kit::assets::Assets).run(move |cx| {
        gpui_kit::init(cx);
        gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| cx.new(|_| Spike)).expect("open window");
    });
}
